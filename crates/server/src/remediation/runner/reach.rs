//! Whether the AI may run commands for an incident, or only diagnose it
//! from what the central server holds (ADR-0175).
//!
//! Every alert gets a diagnosis. Commands run only where an agent
//! accepts them: a server whose agent reports refusing AI commands
//! (ADR-0076), a device without an agent (ADR-0148) and a host that stopped
//! reporting (ADR-0072) get a diagnosis alone, read from the metrics,
//! logs, changes and inventory already collected. Nothing runs on the
//! machine, and the local consent of ADR-0098 is untouched.
//!
//! The answer is recorded in the incident's `context_sent` event, so a
//! resumed incident reads the same answer from its events.

use serde_json::{json, Value};
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::commands::acceptance::{self, Refusal};

/// Why no command can run for an incident: the AI diagnoses only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosisOnly {
    /// The server's agent reports refusing AI commands.
    Refused(Refusal),
    /// A device, which has no agent.
    Device,
    /// The `offline` alert: the host stopped reporting.
    Offline,
}

impl DiagnosisOnly {
    /// Stable code for clients: the refusal's own code (`commands_off`,
    /// ...), `device` or `offline`. A refusal keeps its code, so the
    /// remediation that ends on it gets its new attempt once the server
    /// accepts AI commands (ADR-0165).
    pub fn code(self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.code(),
            Self::Device => "device",
            Self::Offline => "offline",
        }
    }

    /// The value of [`Self::code`] `code`.
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "device" => Some(Self::Device),
            "offline" => Some(Self::Offline),
            other => Refusal::from_code(other).map(Self::Refused),
        }
    }

    /// Why, in an English sentence for people.
    pub fn message(self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.message(),
            Self::Device => "this is a device without an agent, so no command can run on it",
            Self::Offline => "the host stopped reporting, so no command can reach it",
        }
    }

    /// Why, for the AI's instructions.
    pub fn for_prompt(self) -> &'static str {
        match self {
            Self::Refused(_) => {
                "the administrators of this server have not allowed AI commands on it"
            }
            Self::Device => {
                "it is a network or SNMP device without a MonitorFlow agent, checked from outside"
            }
            Self::Offline => {
                "the machine stopped reporting to MonitorFlow; the alert is that silence"
            }
        }
    }

    /// What the `context_sent` event records.
    pub fn payload(self) -> Value {
        json!({ "code": self.code(), "reason": self.message() })
    }

    /// The value recorded by [`Self::payload`] in a `context_sent` payload.
    pub fn of_payload(payload: &Value) -> Option<Self> {
        payload["diagnosis_only"]["code"]
            .as_str()
            .and_then(Self::from_code)
    }
}

/// Why `metric` on `server_id` gets a diagnosis alone, or `None` when the
/// AI may propose commands. A server whose agent never reported what it
/// accepts may: the agent decides in the end (ADR-0076).
pub async fn of(
    db: impl PgExecutor<'_> + Copy,
    server_id: Uuid,
    metric: &str,
) -> Result<Option<DiagnosisOnly>, sqlx::Error> {
    let device: Option<bool> =
        sqlx::query_scalar("SELECT kind = 'device' FROM servers WHERE id = $1")
            .bind(server_id)
            .fetch_optional(db)
            .await?;
    Ok(decide(
        device.unwrap_or(false),
        metric,
        acceptance::shell_refusal(db, server_id).await?,
    ))
}

fn decide(device: bool, metric: &str, refusal: Option<Refusal>) -> Option<DiagnosisOnly> {
    if device {
        Some(DiagnosisOnly::Device)
    } else if metric == "offline" {
        Some(DiagnosisOnly::Offline)
    } else {
        refusal.map(DiagnosisOnly::Refused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_and_silent_hosts_come_before_the_report() {
        let off = Some(Refusal::Off { windows: false });
        let disk = "monitorflow_disk_used_ratio";
        assert_eq!(decide(true, disk, None), Some(DiagnosisOnly::Device));
        assert_eq!(decide(false, "offline", off), Some(DiagnosisOnly::Offline));
        assert_eq!(
            decide(false, disk, off),
            Some(DiagnosisOnly::Refused(Refusal::Off { windows: false }))
        );
        assert_eq!(decide(false, disk, None), None);
    }

    #[test]
    fn the_code_reads_back_and_keeps_the_refusal_codes() {
        let all = [
            DiagnosisOnly::Device,
            DiagnosisOnly::Offline,
            DiagnosisOnly::Refused(Refusal::Off { windows: false }),
            DiagnosisOnly::Refused(Refusal::Off { windows: true }),
            DiagnosisOnly::Refused(Refusal::Unsupported { windows: false }),
            DiagnosisOnly::Refused(Refusal::Unsupported { windows: true }),
        ];
        for why in all {
            assert_eq!(DiagnosisOnly::from_code(why.code()), Some(why));
            let payload = json!({ "diagnosis_only": why.payload() });
            assert_eq!(DiagnosisOnly::of_payload(&payload), Some(why));
        }
        assert_eq!(
            DiagnosisOnly::Refused(Refusal::Off { windows: false }).code(),
            "commands_off"
        );
        assert!(super::super::retry::REFUSAL_CODES.contains(&"commands_off"));
        assert_eq!(DiagnosisOnly::from_code("incomplete"), None);
        assert_eq!(DiagnosisOnly::of_payload(&json!({})), None);
    }
}
