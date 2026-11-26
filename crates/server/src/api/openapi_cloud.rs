//! Cloud accounts in the OpenAPI document (ADR-0183): merged into it by
//! [`super::openapi::build`].

#![allow(dead_code)] // The functions exist for their `#[utoipa::path]`.

use serde_json::json;
use utoipa::OpenApi;

use super::devices;
use super::openapi::{Common, ErrorBody};
use super::openapi_core::ex;
use crate::cloud;

#[derive(OpenApi)]
#[openapi(
    paths(test_cloud, device_cloud, set_device_cloud, delete_device_cloud),
    components(schemas(ErrorBody)),
    modifiers(&Common),
    tags((name = "cloud", description = "Cloud accounts: an AWS account or an Azure subscription is a \
device whose `aws` or `azure` check, run every 5 minutes by the central server alone, lists its virtual \
machines with their state, status checks and basic metrics (processor, network, disks). They join the \
inventory of `GET /api/v1/servers/{id}/virtualization` and are matched to monitored servers as a vCenter's \
are. The credentials are read-only, stored sealed and never returned. Area `servers`."))
)]
pub(super) struct Doc;

fn tested() -> serde_json::Value {
    json!({"login": true, "reason": null, "error": null, "vms": 14, "running": 11, "metrics": true,
           "warning": null})
}

#[utoipa::path(post, path = "/api/v1/devices/cloud/test", tag = "cloud",
    summary = "Test cloud credentials",
    description = "Signs in once from the central server, lists the account's virtual machines and \
reads their metrics, then says what worked. For AWS, a machine-less account still asks CloudWatch, so a \
missing `cloudwatch:GetMetricData` right shows. Nothing is stored.\n\n**Who**: operator or above on the \
site. **Area**: `servers`.",
    request_body(content = devices::CloudTestRequest, examples(
        ("aws" = (summary = "An AWS account in two regions",
            value = json!({"site_id": ex::SITE, "cloud": {"provider": "aws", "key_id": "AKIAIOSFODNN7EXAMPLE",
                           "secret": "a secret access key", "regions": ["eu-west-3", "us-east-1"]}}))),
        ("azure" = (summary = "An Azure subscription read by a service principal",
            value = json!({"site_id": ex::SITE, "cloud": {"provider": "azure",
                           "tenant": "72f988bf-86f1-41af-91ab-2d7cd011db47",
                           "key_id": "1f2e3d4c-5b6a-4789-8a9b-0c1d2e3f4a5b", "secret": "a client secret",
                           "subscription": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"}}))),
    )),
    responses(
        (status = 200, description = "What answered; also when the sign-in failed: `login`, `reason` \
and `error` say so", body = devices::CloudTested, example = json!(tested())),
        (status = 400, description = "Credentials that are not valid: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such site, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn test_cloud() {}

fn view() -> serde_json::Value {
    json!({"provider": "aws", "key_id": "AKIAIOSFODNN7EXAMPLE", "regions": ["eu-west-3", "us-east-1"],
           "tenant": null, "subscription": null, "secret_set": true, "updated_at": "2027-01-02T10:20:00Z"})
}

#[utoipa::path(get, path = "/api/v1/devices/{id}/cloud", tag = "cloud",
    summary = "Read a device's cloud credentials, without the secret",
    description = "**Who**: any role on the device's site. **Area**: `servers`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 200, description = "The credentials", body = cloud::View, example = json!(view())),
        (status = 404, description = "No such device, none set, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn device_cloud() {}

#[utoipa::path(put, path = "/api/v1/devices/{id}/cloud", tag = "cloud",
    summary = "Set a device's cloud credentials",
    description = "Replaces them; what is left out is kept. The secret is kept only while the provider \
and the identity stay the same (the access key ID, or the tenant and application): a kept secret never \
goes to another one. The provider cannot change: remove the credentials first. A device without the \
provider's check gets it, on the central server every 5 minutes, with its alerts: the account cannot be \
read for 10 minutes (`monitorflow_<provider>_up`), and a machine's status check failing for 10 minutes \
(`monitorflow_<provider>_vm_status_checks_failed`), both critical. Thresholds the device has already \
stay.\n\n**Who**: operator or above on the device's site. **Area**: `servers`. Audited as \
`device.cloud_update`, without the secret.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    request_body(content = cloud::CredentialsRequest,
        example = json!({"regions": ["eu-west-3", "eu-central-1"]})),
    responses(
        (status = 200, description = "The credentials, without the secret", body = cloud::View,
            example = json!(ex::merge(view(), json!({"regions": ["eu-west-3", "eu-central-1"]})))),
        (status = 400, description = "Credentials that are not valid: `invalid`", body = ErrorBody),
        (status = 403, description = "The key may only read the site: `forbidden`", body = ErrorBody),
        (status = 404, description = "No such device, or outside the key's reach: `not_found`", body = ErrorBody),
    ))]
fn set_device_cloud() {}

#[utoipa::path(delete, path = "/api/v1/devices/{id}/cloud", tag = "cloud",
    summary = "Remove a device's cloud credentials",
    description = "Its cloud check stops running until credentials are set again.\n\n**Who**: operator \
or above on the device's site. **Area**: `servers`. Audited as `device.cloud_delete`.",
    params(("id" = uuid::Uuid, Path, description = "The device")),
    responses(
        (status = 204, description = "Removed"),
        (status = 404, description = "No such device, none set, or outside the key's reach: `not_found`",
            body = ErrorBody),
    ))]
fn delete_device_cloud() {}
