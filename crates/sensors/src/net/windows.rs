//! Interface traffic on Windows: the IP helper's `GetIfTable2`, the
//! counters behind the Network Interface performance object, read
//! without PDH and its localized counter names (ADR-0149).
//!
//! The instance is the interface's alias, as Windows names it
//! ("Ethernet", "Wi-Fi"). Loopbacks and the filter interfaces that
//! drivers stack on an adapter (which repeat its counters) are left out.

use std::collections::BTreeSet;

use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_ROW2, MIB_IF_TABLE2,
};

use super::{monotonic, select, Counters, NetSensor};
use crate::{Sample, Sensor, SensorError};

/// `IF_TYPE_SOFTWARE_LOOPBACK`.
const LOOPBACK: u32 = 24;
/// Bit of `InterfaceAndOperStatusFlags`: a filter interface.
const FILTER_INTERFACE: u8 = 1 << 1;

/// The table `GetIfTable2` allocated, freed when dropped.
struct Table(*mut MIB_IF_TABLE2);

impl Drop for Table {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a successful GetIfTable2 and is
        // freed once, here, with the function that API documents.
        unsafe { FreeMibTable(self.0.cast()) }
    }
}

fn name(row: &MIB_IF_ROW2) -> String {
    let end = row
        .Alias
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(row.Alias.len());
    String::from_utf16_lossy(&row.Alias[..end])
}

fn read() -> Result<Vec<(String, Counters)>, SensorError> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    // SAFETY: GetIfTable2 writes the address of a table it allocates into
    // `table`, which `Table` frees.
    let status = unsafe { GetIfTable2(&mut table) };
    if status != 0 || table.is_null() {
        return Err(SensorError::Transient(format!(
            "GetIfTable2 failed with error {status}"
        )));
    }
    let table = Table(table);
    // SAFETY: a successful call returns NumEntries rows laid out after the
    // count, as the C declaration's flexible array; the slice lives no
    // longer than `table`. The rows are reached through a raw pointer to
    // the field: a reference to the one-row array would not cover the rest.
    let rows = unsafe {
        let count = (*table.0).NumEntries as usize;
        std::slice::from_raw_parts(
            std::ptr::addr_of!((*table.0).Table).cast::<MIB_IF_ROW2>(),
            count,
        )
    };
    let mut seen = BTreeSet::new();
    let mut interfaces = Vec::new();
    for row in rows {
        if row.Type == LOOPBACK || row.InterfaceAndOperStatusFlags._bitfield & FILTER_INTERFACE != 0
        {
            continue;
        }
        let name = name(row);
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        interfaces.push((
            name,
            Counters {
                receive_bytes: row.InOctets,
                receive_errors: row.InErrors,
                receive_drops: row.InDiscards,
                transmit_bytes: row.OutOctets,
                transmit_errors: row.OutErrors,
                transmit_drops: row.OutDiscards,
            },
        ));
    }
    Ok(interfaces)
}

#[async_trait::async_trait]
impl Sensor for NetSensor {
    async fn collect(&self, _deadline: tokio::time::Instant) -> Result<Vec<Sample>, SensorError> {
        // An in-memory table of the IP stack: quick, no I/O to wait on.
        Ok(self.round(monotonic(), select(read()?)))
    }
}
