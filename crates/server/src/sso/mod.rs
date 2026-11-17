//! Signing in through one's own identity provider (ADR-0145): OpenID
//! Connect providers and LDAP directories over TLS, roles from their
//! groups, accounts made at first sign-in.

pub mod config;
pub mod jws;
pub mod tls;

use crate::api::users::GrantScope;
use crate::api::Role;

/// One group of "Groups and roles": its members get `role` on `scope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub group: String,
    pub role: Role,
    pub scope: GrantScope,
}

/// The grants `groups` give through `mappings`, each once, in the order
/// of the mappings. Group names are compared without regard to case.
pub fn grants(groups: &[String], mappings: &[Mapping]) -> Vec<(Role, GrantScope)> {
    let groups: Vec<String> = groups.iter().map(|g| g.to_lowercase()).collect();
    let mut out: Vec<(Role, GrantScope)> = Vec::new();
    for mapping in mappings {
        let grant = (mapping.role, mapping.scope);
        if groups.contains(&mapping.group.to_lowercase()) && !out.contains(&grant) {
            out.push(grant);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn mapping(group: &str, role: Role, scope: GrantScope) -> Mapping {
        Mapping {
            group: group.into(),
            role,
            scope,
        }
    }

    #[test]
    fn groups_give_their_roles_once() {
        let site = GrantScope::Site(Uuid::new_v4());
        let mappings = [
            mapping("MF-Admins", Role::Administrator, GrantScope::Global),
            mapping("ops", Role::Operator, site),
            mapping("ops-2", Role::Operator, site),
            mapping("viewers", Role::Viewer, GrantScope::Global),
        ];
        let groups = ["mf-admins".to_owned(), "ops".into(), "ops-2".into()];
        assert_eq!(
            grants(&groups, &mappings),
            [
                (Role::Administrator, GrantScope::Global),
                (Role::Operator, site)
            ]
        );
        assert!(grants(&["other".to_owned()], &mappings).is_empty());
        assert!(grants(&[], &mappings).is_empty());
    }
}
