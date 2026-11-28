use super::super::sample;
use super::*;

/// The size and the colour of the pixel at `x`, `y` of a picture.
fn pixel(picture: &Picture, x: u32, y: u32) -> [u8; 3] {
    let decoder = png::Decoder::new(std::io::Cursor::new(picture.png.as_slice()));
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    assert_eq!(
        (info.width, info.height),
        (u32::from(picture.width), u32::from(picture.height))
    );
    let at = ((y * info.width + x) * 3) as usize;
    [buffer[at], buffer[at + 1], buffer[at + 2]]
}

fn activated() -> Screen {
    let mut screen = Screen::new();
    screen
        .apply(Feed::Io(sample::demand_active(64, 48)))
        .unwrap();
    screen
}

#[test]
fn the_first_picture_is_the_whole_screen_then_only_what_changed() {
    let mut screen = activated();
    // The size alone is a change: the replay starts from it.
    assert!(screen.changed());
    screen
        .apply(Feed::Screen(sample::bitmap(4, 8, 2, 2, [200, 100, 50])))
        .unwrap();
    let whole = screen.picture(false).unwrap().unwrap();
    assert_eq!(
        (whole.x, whole.y, whole.width, whole.height),
        (0, 0, 64, 48)
    );
    assert_eq!((whole.screen_width, whole.screen_height), (64, 48));
    assert_eq!(pixel(&whole, 4, 8), [200, 100, 50]);
    assert_eq!(pixel(&whole, 0, 0), [0, 0, 0]);
    assert!(!screen.changed());
    assert_eq!(screen.picture(false).unwrap(), None);

    screen
        .apply(Feed::Screen(sample::bitmap(10, 20, 3, 2, [1, 2, 3])))
        .unwrap();
    let part = screen.picture(false).unwrap().unwrap();
    assert_eq!((part.x, part.y, part.width, part.height), (10, 20, 3, 2));
    assert_eq!(pixel(&part, 0, 0), [1, 2, 3]);
    // Asked for the whole screen: the whole screen, with what changed.
    screen
        .apply(Feed::Screen(sample::bitmap(0, 0, 1, 1, [9, 9, 9])))
        .unwrap();
    let whole = screen.picture(true).unwrap().unwrap();
    assert_eq!((whole.width, whole.height), (64, 48));
    assert_eq!(pixel(&whole, 10, 20), [1, 2, 3]);
}

#[test]
fn a_change_over_most_of_the_screen_is_a_picture_of_all_of_it() {
    let mut screen = activated();
    screen.picture(false).unwrap();
    screen
        .apply(Feed::Screen(sample::bitmap(0, 0, 60, 40, [5, 5, 5])))
        .unwrap();
    let picture = screen.picture(false).unwrap().unwrap();
    assert_eq!((picture.width, picture.height), (64, 48));
}

#[test]
fn a_new_size_starts_again_with_the_whole_screen() {
    let mut screen = activated();
    screen.picture(false).unwrap();
    // A reactivation of the same size keeps the screen.
    screen
        .apply(Feed::Io(sample::demand_active(64, 48)))
        .unwrap();
    assert!(!screen.changed());
    screen
        .apply(Feed::Io(sample::demand_active(32, 16)))
        .unwrap();
    let picture = screen.picture(false).unwrap().unwrap();
    assert_eq!(
        (picture.width, picture.height, picture.screen_width),
        (32, 16, 32)
    );
}

#[test]
fn what_cannot_be_decoded_ends_the_recording() {
    let mut screen = Screen::new();
    let early = screen.apply(Feed::Screen(sample::bitmap(0, 0, 1, 1, [0, 0, 0])));
    assert!(matches!(early, Err(RecordError::Decode(_))), "{early:?}");
    assert!(screen.activate(0, 10, 1).is_err());
    assert!(screen.activate(10_000, 10, 1).is_err());
    // Compressed data the client never asked for.
    let mut screen = activated();
    let mut compressed = sample::bitmap(0, 0, 1, 1, [0, 0, 0]);
    // The update's compression flag (FASTPATH_OUTPUT_COMPRESSION_USED),
    // then its compression byte: compressed, RDP 5.
    let long = compressed[1] & 0x80 != 0;
    let at = if long { 3 } else { 2 };
    compressed[at] |= 0x80;
    compressed.insert(at + 1, 0x21);
    if long {
        let length = u16::from_be_bytes([compressed[1] & 0x7f, compressed[2]]) + 1;
        compressed[1..3].copy_from_slice(&(length | 0x8000).to_be_bytes());
    } else {
        compressed[1] += 1;
    }
    assert!(screen.apply(Feed::Screen(compressed)).is_err());
}

#[test]
fn other_io_pdus_change_nothing() {
    let mut screen = activated();
    screen.picture(false).unwrap();
    // Licensing: a basic security header.
    let license = sample::indication(sample::IO_CHANNEL, vec![0x80, 0, 0, 0, 1, 2, 3]);
    screen.apply(Feed::Io(license)).unwrap();
    screen.apply(Feed::Io(vec![1, 2, 3])).unwrap();
    screen.apply(Feed::Pointer { x: 3, y: 3 }).unwrap();
    assert!(!screen.changed());
}

#[test]
fn a_picture_of_the_whole_screen_counts_as_its_new_size() {
    let mut screen = activated();
    screen.picture(true).unwrap().unwrap();
    screen
        .apply(Feed::Screen(sample::bitmap(4, 8, 2, 2, [200, 100, 50])))
        .unwrap();
    let part = screen.picture(false).unwrap().unwrap();
    assert_eq!((part.x, part.y, part.width, part.height), (4, 8, 2, 2));
}
