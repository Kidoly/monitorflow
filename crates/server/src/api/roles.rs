//! Web interface roles (architecture §9), as stored in `user_roles`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Roles from the `user_roles.role` CHECK constraint (architecture §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    Operator,
    Approver,
    Administrator,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Approver => "approver",
            Self::Administrator => "administrator",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "viewer" => Ok(Self::Viewer),
            "operator" => Ok(Self::Operator),
            "approver" => Ok(Self::Approver),
            "administrator" => Ok(Self::Administrator),
            _ => Err(format!(
                "unknown role `{s}`, expected viewer, operator, approver or administrator"
            )),
        }
    }
}

/// A role granted globally (`site_id` null) or on one site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleGrant {
    pub role: Role,
    pub site_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_through_strings() {
        for role in [
            Role::Viewer,
            Role::Operator,
            Role::Approver,
            Role::Administrator,
        ] {
            assert_eq!(role.as_str().parse::<Role>(), Ok(role));
            assert_eq!(
                serde_json::to_value(role).unwrap(),
                serde_json::json!(role.as_str())
            );
        }
        assert!("admin".parse::<Role>().is_err());
    }
}
