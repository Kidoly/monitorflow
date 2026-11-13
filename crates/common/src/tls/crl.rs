//! The `GetCrl` answer shared by the server and relays (ADR-0025,
//! ADR-0088, ADR-0113).

use crate::proto::v1::GetCrlResponse;

/// Largest `GetCrlResponse` a relay decodes, well above tonic's 4 MiB
/// default: a CRL takes about 35 bytes per revoked certificate, and two
/// travel during a change of certificate authority (ADR-0113).
pub const MAX_CRL_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// `crl_ders`, the active CA's first. `crl_der` repeats the first one only
/// for a caller that does not set `reads_crl_ders`: relays older than
/// ADR-0088 read nothing else, and the copy would otherwise double the size.
pub fn crl_response(crl_ders: Vec<Vec<u8>>, reads_crl_ders: bool) -> GetCrlResponse {
    let crl_der = match reads_crl_ders {
        true => Vec::new(),
        false => crl_ders.first().cloned().unwrap_or_default(),
    };
    GetCrlResponse { crl_der, crl_ders }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_crl_travels_once_for_callers_that_read_the_list() {
        let ders = vec![b"active".to_vec(), b"other".to_vec()];
        let response = crl_response(ders.clone(), true);
        assert!(response.crl_der.is_empty());
        assert_eq!(response.crl_ders, ders);
    }

    #[test]
    fn older_callers_still_get_crl_der() {
        let ders = vec![b"active".to_vec(), b"other".to_vec()];
        let response = crl_response(ders.clone(), false);
        assert_eq!(response.crl_der, b"active");
        assert_eq!(response.crl_ders, ders);
        assert!(crl_response(Vec::new(), false).crl_der.is_empty());
    }
}
