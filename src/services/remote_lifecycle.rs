//! Removing a remote: the DB cascade and the cleanup of its rclone config or Proton session, through the Repository and BackendClient ports. UI removal is the caller's problem.

use crate::{
    domain::{
        ports::{BackendClient, Repository},
        remote::Backend,
    },
    services::auth,
    util,
};

/// Delete a remote by name: first its credentials (rclone config section or Proton session), then its row with its folders and their items. In this order a failure leaves a remote that asks for a sign-in, never credentials nobody sees.
pub fn delete_remote(
    remote_name: &str,
    repo: &dyn Repository,
    client: &dyn BackendClient,
) -> Result<(), String> {
    let remote = util::await_future(repo.find_remote_by_name(remote_name))
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("remote '{remote_name}' not found"))?;
    if remote.backend == Backend::NativeProton {
        // A keyring entry, no rclone config section.
        syncative_go::proton::unbind_remote(remote_name);
        auth::forget_proton_session(remote_name)?;
    } else {
        client.delete_config(remote_name)?;
    }
    util::await_future(repo.cascade_delete_remote(remote.id)).map_err(|e| e.to_string())
}
