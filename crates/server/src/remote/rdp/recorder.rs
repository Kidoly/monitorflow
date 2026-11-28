//! Desktop recordings (ADR-0192): the screen of a recorded desktop,
//! decoded on the central server from the service's PDUs as they pass,
//! kept as PNG pictures in `remote_desktop_frames`.
//!
//! [`Screen`] runs IronRDP's decoders, the ones its web client runs, on
//! the fast path and the slow-path graphics, and knows which part of the
//! screen changed. [`record`] takes a picture of it at most once a
//! second, only of the part that changed, and of the whole screen at the
//! start, when its size changes and once a minute while it changes, so a
//! replay can start anywhere. A recording stops at [`MAX_BYTES`]: the
//! session then ends, as a recorded session must be recorded.
//!
//! Nothing the person types is kept: the screen shows what it showed,
//! and a password typed in a field shows as Windows draws it.

use std::time::Duration;

use ironrdp_bulk::{BulkCompressor, CompressionType};
use ironrdp_core::{ReadCursor, WriteBuf};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::mcs;
use ironrdp_pdu::rdp::capability_sets::CapabilitySet;
use ironrdp_pdu::rdp::headers::{
    decode_share_control, ShareControlPdu, ShareDataHeader, ShareDataPdu,
};
use ironrdp_pdu::slow_path::{self, GraphicsUpdateType};
use ironrdp_session::fast_path::{Processor, ProcessorBuilder, UpdateKind};
use ironrdp_session::image::DecodedImage;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};
use uuid::Uuid;

/// Time between two pictures, at most.
pub const EVERY: Duration = Duration::from_secs(1);
/// Time between two pictures of the whole screen while it changes.
pub const WHOLE_EVERY: Duration = Duration::from_secs(60);
/// The most a recording keeps, in bytes of pictures.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// The largest desktop recorded, each side.
const MAX_SIDE: u16 = 8192;
/// What reaches the recorder at once, at most.
const BATCH: usize = 64;

/// A PDU, or what it said, for the recorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    /// A fast-path PDU of the service.
    Screen(Vec<u8>),
    /// A Send Data Indication of the service on the I/O channel.
    Io(Vec<u8>),
    /// The client moved the pointer.
    Pointer { x: u16, y: u16 },
    /// The bulk compression the client asked for.
    Compression(Option<CompressionType>),
}

/// Why a recording could not go on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    #[error("the recording could not read the screen: {0}")]
    Decode(String),
    #[error("the recording reached its limit of {} MiB; open a new session to go on", MAX_BYTES / (1024 * 1024))]
    Full,
}

fn decode_error(error: impl std::fmt::Display) -> RecordError {
    RecordError::Decode(error.to_string())
}

/// One picture: a part of the screen, or all of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// The size of the whole screen when it was taken.
    pub screen_width: u16,
    pub screen_height: u16,
    pub png: Vec<u8>,
}

/// The screen as the client sees it, decoded.
pub struct Screen {
    image: Option<DecodedImage>,
    processor: Option<Processor>,
    bulk: Option<BulkCompressor>,
    /// What changed since the last picture.
    dirty: Option<InclusiveRectangle>,
    /// The next picture is of the whole screen: its size changed.
    resized: bool,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    pub fn new() -> Self {
        Self {
            image: None,
            processor: None,
            bulk: None,
            dirty: None,
            resized: false,
        }
    }

    /// Whether a picture would show something new.
    pub fn changed(&self) -> bool {
        self.image.is_some() && (self.resized || self.dirty.is_some())
    }

    /// The desktop is `width` by `height`, as the service's Demand Active
    /// says, at the start and after each reactivation.
    pub fn activate(&mut self, width: u16, height: u16, share_id: u32) -> Result<(), RecordError> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(RecordError::Decode(format!(
                "a desktop of {width} by {height} pixels"
            )));
        }
        let same = self
            .image
            .as_ref()
            .is_some_and(|i| (i.width(), i.height()) == (width, height));
        if !same {
            self.image = Some(DecodedImage::new(PixelFormat::RgbA32, width, height));
            self.resized = true;
            self.dirty = None;
        }
        // A new activation starts the codecs' state again.
        self.processor = Some(
            ProcessorBuilder {
                io_channel_id: 0,
                user_channel_id: 0,
                share_id,
                enable_server_pointer: true,
                pointer_software_rendering: true,
            }
            .build(),
        );
        Ok(())
    }

    pub fn apply(&mut self, feed: Feed) -> Result<(), RecordError> {
        match feed {
            Feed::Compression(compression) => {
                self.bulk = compression.map(BulkCompressor::new);
                Ok(())
            }
            Feed::Pointer { x, y } => {
                if let Some(processor) = self.processor.as_mut() {
                    processor.update_mouse_pos(x, y);
                }
                Ok(())
            }
            Feed::Screen(frame) => {
                let (Some(image), Some(processor)) = (self.image.as_mut(), self.processor.as_mut())
                else {
                    return Err(RecordError::Decode(
                        "the screen came before the desktop's size".into(),
                    ));
                };
                let updates = processor
                    .process(image, &frame, &mut WriteBuf::new(), &mut self.bulk)
                    .map_err(decode_error)?;
                self.updated(updates);
                Ok(())
            }
            Feed::Io(frame) => self.io(&frame),
        }
    }

    fn updated(&mut self, updates: Vec<UpdateKind>) {
        for update in updates {
            if let UpdateKind::Region(region) = update {
                self.dirty = Some(match self.dirty.take() {
                    Some(dirty) => union(&dirty, &region),
                    None => region,
                });
            }
        }
    }

    /// A Send Data Indication on the I/O channel: the Demand Active, and
    /// slow-path graphics. The rest goes through the bulk decompressor
    /// only, whose history the fast path shares.
    fn io(&mut self, frame: &[u8]) -> Result<(), RecordError> {
        let Ok(data) = mcs::decode_send_data_indication(frame) else {
            return Ok(());
        };
        let user_data = data.user_data;
        // Licensing and the like have a basic security header, whose
        // second word is 0; a Share Control header has its PDU type there.
        if user_data.get(2..4) == Some(&[0, 0][..]) {
            return Ok(());
        }
        // Several Share Control PDUs may follow one another.
        let mut offset = 0;
        while offset + 2 <= user_data.len() {
            let length = usize::from(u16::from_le_bytes([
                user_data[offset],
                user_data[offset + 1],
            ]));
            if length == 0 || offset + length > user_data.len() {
                break;
            }
            let part = mcs::SendDataIndicationCtx {
                user_data: &user_data[offset..offset + length],
                ..data
            };
            offset += length;
            let Ok(control) = decode_share_control(part) else {
                continue;
            };
            match control.pdu {
                ShareControlPdu::ServerDemandActive(demand) => {
                    let size = demand.pdu.capability_sets.iter().find_map(|set| match set {
                        CapabilitySet::Bitmap(b) => Some((b.desktop_width, b.desktop_height)),
                        _ => None,
                    });
                    let Some((width, height)) = size else {
                        return Err(RecordError::Decode(
                            "the desktop did not give its size".into(),
                        ));
                    };
                    self.activate(width, height, control.share_id)?;
                }
                ShareControlPdu::Data(header) => self.share_data(header)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn share_data(&mut self, header: ShareDataHeader) -> Result<(), RecordError> {
        let ShareDataHeader {
            share_data_pdu,
            compression_flags,
            compression_type,
            ..
        } = header;
        let flags = u32::from(compression_flags.bits()) | u32::from(compression_type.as_u8());
        let (pdu, raw) = match share_data_pdu {
            ShareDataPdu::Compressed { pdu_type, data } => {
                let data = self.decompress(&data, flags)?;
                let pdu = ShareDataPdu::decode_with_type(&data, pdu_type).map_err(decode_error)?;
                (pdu, true)
            }
            pdu => (pdu, compression_flags.is_empty()),
        };
        let data = match pdu {
            ShareDataPdu::Update(data) | ShareDataPdu::Pointer(data) if !raw => {
                self.decompress(&data, flags)?
            }
            ShareDataPdu::Update(data) | ShareDataPdu::Pointer(data) => data,
            _ => return Ok(()),
        };
        let (Some(image), Some(processor)) = (self.image.as_mut(), self.processor.as_mut()) else {
            return Ok(());
        };
        let mut src = ReadCursor::new(&data);
        // A slow-path pointer update starts with its own type, never a
        // graphics update's; try the graphics first.
        let updates = match slow_path::read_graphics_update_type(&mut src) {
            Ok(GraphicsUpdateType::Bitmap) => {
                let bitmap = slow_path::decode_slow_path_bitmap(&mut src).map_err(decode_error)?;
                processor
                    .process_bitmap_update(image, bitmap)
                    .map_err(decode_error)?
            }
            Ok(_) => Vec::new(),
            Err(_) => {
                let mut src = ReadCursor::new(&data);
                match slow_path::decode_slow_path_pointer(&mut src) {
                    Ok(pointer) => processor
                        .process_pointer_update(image, pointer)
                        .map_err(decode_error)?,
                    Err(_) => Vec::new(),
                }
            }
        };
        self.updated(updates);
        Ok(())
    }

    fn decompress(&mut self, data: &[u8], flags: u32) -> Result<Vec<u8>, RecordError> {
        let bulk = self.bulk.as_mut().ok_or_else(|| {
            RecordError::Decode("compressed data the client did not ask for".into())
        })?;
        bulk.decompress(data, flags)
            .map(<[u8]>::to_vec)
            .map_err(decode_error)
    }

    /// A picture of what changed, or of the whole screen when `whole` or
    /// its size changed; `None` when nothing did.
    pub fn picture(&mut self, whole: bool) -> Result<Option<Picture>, RecordError> {
        if !self.changed() {
            return Ok(None);
        }
        let Some(image) = self.image.as_ref() else {
            return Ok(None);
        };
        let dirty = self.dirty.take();
        let resized = std::mem::take(&mut self.resized);
        let whole = whole || resized;
        let (width, height) = (image.width(), image.height());
        let full = InclusiveRectangle {
            left: 0,
            top: 0,
            right: width - 1,
            bottom: height - 1,
        };
        let part = match dirty {
            _ if whole => full,
            None => return Ok(None),
            Some(dirty) => {
                let part = InclusiveRectangle {
                    left: dirty.left.min(width - 1),
                    top: dirty.top.min(height - 1),
                    right: dirty.right.min(width - 1),
                    bottom: dirty.bottom.min(height - 1),
                };
                // Most of the screen: the whole of it, for the replay to
                // start from.
                if area(&part) * 2 >= area(&full) {
                    full
                } else {
                    part
                }
            }
        };
        let png = encode(image, &part)?;
        Ok(Some(Picture {
            x: part.left,
            y: part.top,
            width: part.right - part.left + 1,
            height: part.bottom - part.top + 1,
            screen_width: width,
            screen_height: height,
            png,
        }))
    }
}

fn union(a: &InclusiveRectangle, b: &InclusiveRectangle) -> InclusiveRectangle {
    InclusiveRectangle {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn area(r: &InclusiveRectangle) -> u64 {
    u64::from(r.right.saturating_sub(r.left) + 1) * u64::from(r.bottom.saturating_sub(r.top) + 1)
}

/// The part `rect` of `image` as a PNG, without its alpha channel.
fn encode(image: &DecodedImage, rect: &InclusiveRectangle) -> Result<Vec<u8>, RecordError> {
    let width = usize::from(rect.right - rect.left + 1);
    let height = usize::from(rect.bottom - rect.top + 1);
    let stride = image.stride();
    let bpp = image.bytes_per_pixel();
    let data = image.data();
    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in usize::from(rect.top)..=usize::from(rect.bottom) {
        let start = row * stride + usize::from(rect.left) * bpp;
        for pixel in data[start..start + width * bpp].chunks_exact(bpp) {
            rgb.extend_from_slice(&pixel[..3]);
        }
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(decode_error)?;
    writer.write_image_data(&rgb).map_err(decode_error)?;
    writer.finish().map_err(decode_error)?;
    Ok(png)
}

/// Where the pictures go.
#[derive(Clone)]
pub struct Store {
    pub pool: PgPool,
    pub session: Uuid,
}

impl Store {
    async fn write(&self, seq: i32, at_ms: i64, picture: &Picture) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO remote_desktop_frames (session_id, seq, at_ms, x, y, width, height,
                 screen_width, screen_height, image)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(self.session)
        .bind(seq)
        .bind(at_ms)
        .bind(i32::from(picture.x))
        .bind(i32::from(picture.y))
        .bind(i32::from(picture.width))
        .bind(i32::from(picture.height))
        .bind(i32::from(picture.screen_width))
        .bind(i32::from(picture.screen_height))
        .bind(&picture.png)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }
}

/// Records what `feed` brings until it closes, writing pictures to
/// `store`, times counted from `started`. Returns why it could not go
/// on, if it could not: the session then ends.
pub async fn record(
    store: Store,
    mut feed: mpsc::Receiver<Feed>,
    started: Instant,
) -> Result<(), RecordError> {
    let mut screen = Box::new(Screen::new());
    let mut changed = false;
    let mut seq = 0i32;
    let mut stored = 0u64;
    let mut last: Option<Instant> = None;
    let mut last_whole: Option<Instant> = None;
    let mut open = true;
    while open {
        let due = last.map_or(started, |at| at + EVERY);
        let mut batch = Vec::new();
        tokio::select! {
            next = feed.recv() => match next {
                Some(first) => {
                    batch.push(first);
                    while batch.len() < BATCH {
                        match feed.try_recv() {
                            Ok(next) => batch.push(next),
                            Err(_) => break,
                        }
                    }
                }
                None => open = false,
            },
            () = sleep_until(due), if changed => {}
        }
        let now = Instant::now();
        // The last picture when the session ends, whatever the time.
        let take = changed_after(&batch, changed) && (now >= due || !open);
        let whole = take && last_whole.is_none_or(|at| now >= at + WHOLE_EVERY);
        let (back, result) = tokio::task::spawn_blocking(move || {
            let result = batch
                .into_iter()
                .try_for_each(|f| screen.apply(f))
                .and_then(|()| {
                    if take || (!open && screen.changed()) {
                        screen.picture(whole)
                    } else {
                        Ok(None)
                    }
                });
            (screen, result)
        })
        .await
        .map_err(|_| RecordError::Decode("the decoder stopped".into()))?;
        screen = back;
        changed = screen.changed();
        if let Some(picture) = result? {
            stored += picture.png.len() as u64;
            if stored > MAX_BYTES {
                return Err(RecordError::Full);
            }
            let at_ms = now.duration_since(started).as_millis() as i64;
            if let Err(error) = store.write(seq, at_ms, &picture).await {
                tracing::error!(session = %store.session, %error, "cannot write a desktop recording's picture");
            }
            seq += 1;
            last = Some(now);
            if (picture.width, picture.height) == (picture.screen_width, picture.screen_height) {
                last_whole = Some(now);
            }
        }
    }
    Ok(())
}

/// Whether a picture may be due: the screen changed, or `batch` may change
/// it.
fn changed_after(batch: &[Feed], changed: bool) -> bool {
    changed || !batch.is_empty()
}

#[cfg(test)]
mod tests;
