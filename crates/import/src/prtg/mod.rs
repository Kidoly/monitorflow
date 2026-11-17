//! The PRTG reader: through its HTTP API, with an API token or a user and
//! passhash, or from its configuration file (`PRTG Configuration.dat`).
//! Both give a [`tree::Tree`] of probes, groups, devices and sensors;
//! [`sensors`] says what each sensor becomes in MonitorFlow.

pub mod sensors;
pub mod tree;
