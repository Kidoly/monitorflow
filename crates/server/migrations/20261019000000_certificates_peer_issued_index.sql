-- Renewal (ADR-0010) refuses a peer whose latest certificate was issued
-- too recently, so it looks up the newest issued_at per peer, revoked
-- certificates included.
CREATE INDEX certificates_peer_issued_idx ON certificates (peer_role, peer_id, issued_at);
