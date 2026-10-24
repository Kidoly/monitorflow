//! Tenancy enforcement (ADR-0026): the one place that turns a user's role
//! grants into the sites and customers a request may touch.
//!
//! Handlers never look at grants. They ask [`CurrentUser::readable_sites`]
//! or [`CurrentUser::modifiable_sites`] for a [`Scope`] to filter queries
//! with, or call [`authorize`] for one resource. Both go through
//! [`Access::sites`], which accounts for global, customer and site grants.

use std::collections::BTreeSet;

use sqlx::PgExecutor;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::roles::{Role, RoleGrant};
use super::ApiError;

/// A set of site or customer IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every one, present and future.
    All,
    /// Only these, sorted and without duplicates; possibly none.
    Only(Vec<Uuid>),
}

impl Scope {
    pub fn contains(&self, id: Uuid) -> bool {
        match self {
            Self::All => true,
            Self::Only(ids) => ids.binary_search(&id).is_ok(),
        }
    }

    /// `(all, ids)` to bind to a `($1 OR column = ANY($2))` filter.
    pub fn bind(&self) -> (bool, Vec<Uuid>) {
        match self {
            Self::All => (true, Vec::new()),
            Self::Only(ids) => (false, ids.clone()),
        }
    }
}

/// What one grant reaches, with a customer grant expanded to the sites the
/// customer had when the request started.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Coverage {
    All,
    Customer { customer: Uuid, sites: Vec<Uuid> },
    Site(Uuid),
}

/// A user's grants, resolved to sites once per request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Access {
    grants: Vec<(Role, Coverage)>,
}

impl Access {
    /// Resolves `grants`, reading the sites of the customers they name.
    pub async fn load<'e>(
        db: impl PgExecutor<'e>,
        grants: &[RoleGrant],
    ) -> Result<Self, sqlx::Error> {
        let customers: Vec<Uuid> = grants.iter().filter_map(|g| g.customer_id).collect();
        let sites: Vec<(Uuid, Uuid)> = if customers.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as("SELECT customer_id, id FROM sites WHERE customer_id = ANY($1)")
                .bind(&customers)
                .fetch_all(db)
                .await?
        };
        Ok(Self::resolve(grants, &sites))
    }

    /// `grants` with `customer_sites` as `(customer, site)` pairs.
    fn resolve(grants: &[RoleGrant], customer_sites: &[(Uuid, Uuid)]) -> Self {
        let grants = grants
            .iter()
            .map(|grant| {
                let coverage = match (grant.customer_id, grant.site_id) {
                    (Some(customer), _) => Coverage::Customer {
                        customer,
                        sites: customer_sites
                            .iter()
                            .filter(|(c, _)| *c == customer)
                            .map(|(_, site)| *site)
                            .collect(),
                    },
                    (None, Some(site)) => Coverage::Site(site),
                    (None, None) => Coverage::All,
                };
                (grant.role, coverage)
            })
            .collect();
        Self { grants }
    }

    /// Whether the user has any grant at all.
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// Sites reached by the grants whose role passes `allowed`.
    pub fn sites(&self, allowed: impl Fn(Role) -> bool) -> Scope {
        let mut sites = BTreeSet::new();
        for (_, coverage) in self.grants.iter().filter(|(role, _)| allowed(*role)) {
            match coverage {
                Coverage::All => return Scope::All,
                Coverage::Customer { sites: s, .. } => sites.extend(s),
                Coverage::Site(site) => {
                    sites.insert(*site);
                }
            }
        }
        Scope::Only(sites.into_iter().collect())
    }

    /// Sites reached by global and customer grants whose role passes
    /// `allowed`; site grants are left out.
    pub fn customer_sites(&self, allowed: impl Fn(Role) -> bool) -> Scope {
        let whole = Access {
            grants: self
                .grants
                .iter()
                .filter(|(_, coverage)| !matches!(coverage, Coverage::Site(_)))
                .cloned()
                .collect(),
        };
        whole.sites(allowed)
    }

    /// Customers granted as a whole to roles passing `allowed`; customers
    /// reached only through a site grant are not included.
    pub fn customers(&self, allowed: impl Fn(Role) -> bool) -> Scope {
        let mut customers = BTreeSet::new();
        for (_, coverage) in self.grants.iter().filter(|(role, _)| allowed(*role)) {
            match coverage {
                Coverage::All => return Scope::All,
                Coverage::Customer { customer, .. } => {
                    customers.insert(*customer);
                }
                Coverage::Site(_) => {}
            }
        }
        Scope::Only(customers.into_iter().collect())
    }
}

impl CurrentUser {
    /// Every role includes read access to the sites it covers.
    pub fn can_read(&self) -> bool {
        !self.access.is_empty()
    }

    /// Sites whose data the user may read.
    pub fn readable_sites(&self) -> Scope {
        self.access.sites(|_| true)
    }

    /// Sites whose configuration the user may change (operator or above).
    pub fn modifiable_sites(&self) -> Scope {
        self.access.sites(Role::can_modify)
    }

    /// Sites where the user holds `role` or a role above it, by any grant.
    pub fn sites_with(&self, role: Role) -> Scope {
        self.access.sites(|r| r >= role)
    }

    /// Customers granted to the user as a whole, by any role. Customers of
    /// readable sites are visible too; see `customers::list`.
    pub fn visible_customers(&self) -> Scope {
        self.access.customers(|_| true)
    }

    /// Customers the user administers as a whole, where it may add sites.
    pub fn administered_customers(&self) -> Scope {
        self.access.customers(|role| role == Role::Administrator)
    }

    /// Sites of the customers the user administers as a whole, globally or
    /// by customer grant: where it may enroll and revoke peers (ADR-0029).
    /// A site administrator grant is not enough.
    pub fn administered_sites(&self) -> Scope {
        self.access
            .customer_sites(|role| role == Role::Administrator)
    }
}

/// The site `site` if the user may read it (and change it, with
/// `modify`). A user without any role gets 403; a missing or unreadable
/// site 404, so the API does not reveal which IDs exist; a readable one
/// the user may not change 403.
pub(super) fn authorize(
    user: &CurrentUser,
    site: Option<Uuid>,
    modify: bool,
) -> Result<Uuid, ApiError> {
    if !user.can_read() {
        return Err(ApiError::Forbidden);
    }
    let site = site
        .filter(|s| user.readable_sites().contains(*s))
        .ok_or(ApiError::NotFound)?;
    if modify && !user.modifiable_sites().contains(site) {
        return Err(ApiError::Forbidden);
    }
    Ok(site)
}

/// Like [`authorize`] for reading, then 403 unless the user administers
/// the site's customer ([`CurrentUser::administered_sites`]).
pub(super) fn authorize_admin(user: &CurrentUser, site: Option<Uuid>) -> Result<Uuid, ApiError> {
    let site = authorize(user, site, false)?;
    if !user.administered_sites().contains(site) {
        return Err(ApiError::Forbidden);
    }
    Ok(site)
}

#[cfg(test)]
mod tests {
    use super::*;

    const C1: Uuid = Uuid::from_u128(0xc1);
    const C2: Uuid = Uuid::from_u128(0xc2);

    fn site(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Sites 1 and 2 belong to C1, site 3 to C2.
    fn access(grants: &[(Role, Option<Uuid>, Option<Uuid>)]) -> Access {
        let grants: Vec<RoleGrant> = grants
            .iter()
            .map(|&(role, customer_id, site_id)| RoleGrant {
                role,
                customer_id,
                site_id,
            })
            .collect();
        let sites = [(C1, site(1)), (C1, site(2)), (C2, site(3))];
        Access::resolve(&grants, &sites)
    }

    fn only(ids: &[Uuid]) -> Scope {
        Scope::Only(ids.to_vec())
    }

    #[test]
    fn global_grants_cover_every_site() {
        let a = access(&[
            (Role::Viewer, None, Some(site(1))),
            (Role::Viewer, None, None),
        ]);
        assert_eq!(a.sites(|_| true), Scope::All);
        assert_eq!(a.customers(|_| true), Scope::All);
        assert_eq!(a.sites(Role::can_modify), only(&[]));
        assert!(Scope::All.contains(site(9)));
    }

    #[test]
    fn customer_grants_cover_the_customer_sites_only() {
        let a = access(&[(Role::Operator, Some(C1), None)]);
        assert_eq!(a.sites(|_| true), only(&[site(1), site(2)]));
        assert_eq!(a.sites(Role::can_modify), only(&[site(1), site(2)]));
        assert!(!a.sites(|_| true).contains(site(3)));
        assert_eq!(a.customers(|_| true), only(&[C1]));
    }

    #[test]
    fn grants_combine_per_role() {
        let a = access(&[
            (Role::Viewer, Some(C1), None),
            (Role::Operator, None, Some(site(2))),
            (Role::Viewer, None, Some(site(3))),
        ]);
        assert_eq!(a.sites(|_| true), only(&[site(1), site(2), site(3)]));
        assert_eq!(a.sites(Role::can_modify), only(&[site(2)]));
        // A site grant does not make its customer visible as a whole.
        assert_eq!(a.customers(|_| true), only(&[C1]));
        assert_eq!(a.customers(Role::can_modify), only(&[]));
    }

    #[test]
    fn a_customer_without_sites_covers_none_and_no_grant_is_empty() {
        let a = access(&[(Role::Administrator, Some(Uuid::from_u128(0xc3)), None)]);
        assert_eq!(a.sites(|_| true), only(&[]));
        assert!(!a.is_empty());
        let none = access(&[]);
        assert!(none.is_empty());
        assert_eq!(none.sites(|_| true), only(&[]));
        assert_eq!(only(&[site(1)]).bind(), (false, vec![site(1)]));
        assert_eq!(Scope::All.bind(), (true, vec![]));
    }

    #[test]
    fn authorize_keeps_404_for_hidden_and_403_for_read_only() {
        let user = |grants| CurrentUser::for_tests(access(grants));
        let viewer = user(&[(Role::Viewer, Some(C1), None)]);
        assert_eq!(authorize(&viewer, Some(site(1)), false).unwrap(), site(1));
        assert!(matches!(
            authorize(&viewer, Some(site(1)), true),
            Err(ApiError::Forbidden)
        ));
        assert!(matches!(
            authorize(&viewer, Some(site(3)), false),
            Err(ApiError::NotFound)
        ));
        assert!(matches!(
            authorize(&viewer, None, false),
            Err(ApiError::NotFound)
        ));
        let operator = user(&[(Role::Viewer, None, None), (Role::Operator, Some(C2), None)]);
        assert!(authorize(&operator, Some(site(3)), true).is_ok());
        assert!(matches!(
            authorize(&operator, Some(site(1)), true),
            Err(ApiError::Forbidden)
        ));
        let nobody = user(&[]);
        assert!(matches!(
            authorize(&nobody, Some(site(1)), false),
            Err(ApiError::Forbidden)
        ));
    }

    #[test]
    fn administration_needs_a_customer_or_global_administrator_grant() {
        let user =
            |grants: &[(Role, Option<Uuid>, Option<Uuid>)]| CurrentUser::for_tests(access(grants));
        let customer = user(&[(Role::Administrator, Some(C1), None)]);
        assert_eq!(customer.administered_sites(), only(&[site(1), site(2)]));
        assert_eq!(authorize_admin(&customer, Some(site(2))).unwrap(), site(2));
        assert!(matches!(
            authorize_admin(&customer, Some(site(3))),
            Err(ApiError::NotFound)
        ));
        let site_admin = user(&[(Role::Administrator, None, Some(site(1)))]);
        assert_eq!(site_admin.administered_sites(), only(&[]));
        assert!(matches!(
            authorize_admin(&site_admin, Some(site(1))),
            Err(ApiError::Forbidden)
        ));
        let operator = user(&[(Role::Operator, None, None)]);
        assert!(matches!(
            authorize_admin(&operator, Some(site(1))),
            Err(ApiError::Forbidden)
        ));
        let global = user(&[(Role::Administrator, None, None)]);
        assert_eq!(global.administered_sites(), Scope::All);
    }
}
