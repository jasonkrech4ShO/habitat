use super::{ExecutionStrategy,
            Scope};
use crate::{command::pkg::list,
            config,
            error::{Error,
                    Result}};
use futures::stream::StreamExt;
use habitat_common::{package_graph::PackageGraph,
                     types::ListenCtlAddr,
                     ui::{Status,
                          UIWriter}};
use habitat_core::{error as herror,
                   fs::{self as hfs,
                        FS_ROOT_PATH},
                   package::{list::temp_package_directory,
                             Identifiable,
                             PackageIdent,
                             PackageInstall}};
use habitat_sup_client::{SrvClient,
                         SrvClientError};
use habitat_sup_protocol;
use std::{fs,
          path::Path,
          str::FromStr};

/// `Force` indictates that the package should be uninstalled even if it is loaded by the
/// supervisor. This only applies to packages specified in `idents`. It does not apply to their
/// dependencies.
#[derive(Clone, Copy)]
pub enum UninstallSafety {
    Safe,
    Force,
}

pub async fn uninstall<U>(ui: &mut U,
                          ident: impl AsRef<PackageIdent>,
                          fs_root_path: &Path,
                          execution_strategy: ExecutionStrategy,
                          scope: Scope,
                          excludes: &[PackageIdent],
                          safety: UninstallSafety)
                          -> Result<()>
    where U: UIWriter
{
    uninstall_many(ui,
                   &[ident],
                   fs_root_path,
                   execution_strategy,
                   scope,
                   excludes,
                   safety).await
}

/// Uninstall all but the `number_latest_to_keep` packages.
///
/// Returns the number of packages that were uninstalled
#[allow(clippy::too_many_arguments)]
pub async fn uninstall_all_but_latest<U>(ui: &mut U,
                                         ident: impl AsRef<PackageIdent>,
                                         number_latest_to_keep: usize,
                                         fs_root_path: &Path,
                                         execution_strategy: ExecutionStrategy,
                                         scope: Scope,
                                         excludes: &[PackageIdent],
                                         safety: UninstallSafety)
                                         -> Result<usize>
    where U: UIWriter
{
    let ident = ident.as_ref();
    let mut idents = list::package_list(&ident.clone().into())?;
    if number_latest_to_keep >= idents.len() {
        ui.begin(format!("Uninstalling {}", ident))?;
        ui.status(Status::Skipping,
                  format!("Only {} packages installed", idents.len()))?;
        ui.end(format!("Uninstall of {} complete", ident))?;
        return Ok(0);
    }
    // Reverse sort the idents so the latest occur first in the list
    idents.sort_unstable_by(|a, b| b.by_parts_cmp(a));
    let to_uninstall = &idents[number_latest_to_keep..];
    uninstall_many(ui,
                   to_uninstall,
                   fs_root_path,
                   execution_strategy,
                   scope,
                   excludes,
                   safety).await?;
    Ok(to_uninstall.len())
}

/// Delete packages and all dependencies which are not used by the packages.
/// We do an ordered traverse of the dependencies, updating the graph as we delete a
/// package. This lets us use simple logic where we continually check if the package
/// we're trying to delete currently has no packages depending on it.
///
/// The full logic is:
/// 1. We find all packages on the filesystem and convert them into a graph
/// 2. We find the fully qualified package ident and all its dependencies
/// 3. We do a BFS on the graph to get the dependencies in order
/// 4. We check if the specified package has any reverse deps
///     4a. If there are, we throw an error
///     4b. If not, we delete the package
/// 5. For each dependency we check if there are any packages which depend on it
///     5a. If there are, we skip it
///     5b. If there are not, we delete it from disk and the graph
///
/// `excludes` is a list of user-supplied `PackageIdent`s.
pub async fn uninstall_many<U>(ui: &mut U,
                               idents: &[impl AsRef<PackageIdent>],
                               fs_root_path: &Path,
                               execution_strategy: ExecutionStrategy,
                               scope: Scope,
                               excludes: &[PackageIdent],
                               safety: UninstallSafety)
                               -> Result<()>
    where U: UIWriter
{
    // 1.
    let mut graph = PackageGraph::from_root_path(fs_root_path)?;

    let loaded_services = supervisor_services().await?;
    if !loaded_services.is_empty() {
        ui.status(Status::Determining, "list of loaded services in supervisor")?;
        for s in loaded_services.iter() {
            ui.status(Status::Found, format!("loaded service {}", s))?;
        }
    }
    let safety = match safety {
        UninstallSafety::Safe => UninstallSafetyImpl::SkipIfLoaded(&loaded_services),
        UninstallSafety::Force => UninstallSafetyImpl::Force,
    };
    // Never uninstall a dependency if it is loaded
    let dependency_safety = UninstallSafetyImpl::SkipIfLoaded(&loaded_services);

    for ident in idents {
        // 2.
        let ident = ident.as_ref();
        let pkg_install = PackageInstall::load(ident, Some(fs_root_path))?;
        let ident = pkg_install.ident();
        ui.begin(format!("Uninstalling {}", &ident))?;

        // 3.
        let deps = graph.owned_ordered_deps(&ident);

        // 4.
        match graph.count_rdeps(&ident) {
            None => {
                // package not in graph - this shouldn't happen but could be a race condition in
                // Step 2 with another hab uninstall. We can continue as what we
                // wanted (package to be removed) has already happened. We're going
                // to continue and try and delete down through the dependency tree
                // anyway
                ui.warn(format!("Tried to find dependant packages of {} but it wasn't in \
                                 graph.  Maybe another uninstall command was run at the same \
                                 time?",
                                &ident))?;
            }
            Some(0) => {
                maybe_delete(ui,
                             &fs_root_path,
                             &pkg_install,
                             execution_strategy,
                             &excludes,
                             safety)?;
                graph.remove(&ident);
            }
            Some(c) => {
                return Err(Error::CannotRemovePackage(ident.clone(), c));
            }
        }

        // 5.
        let mut count = 0;
        match scope {
            Scope::Package => {
                ui.status(Status::Skipping, "dependencies (--no-deps specified)")?;
            }
            Scope::PackageAndDependencies => {
                for p in &deps {
                    match graph.count_rdeps(&p) {
                        None => {
                            // package not in graph - this shouldn't happen but could be a race
                            // condition in Step 2 with another hab uninstall. We can
                            // continue as what we wanted (package to be removed) has already
                            // happened. We're going to continue and try
                            // and delete down through the dependency
                            // tree anyway
                            ui.warn(format!("Tried to find dependant packages of {} but it \
                                             wasn't in graph.  Maybe another uninstall command \
                                             was run at the same time?",
                                            &p))?;
                        }
                        Some(0) => {
                            let install = PackageInstall::load(&p, Some(fs_root_path))?;
                            maybe_delete(ui,
                                         &fs_root_path,
                                         &install,
                                         execution_strategy,
                                         &excludes,
                                         dependency_safety)?;

                            graph.remove(&p);
                            count += 1;
                        }
                        Some(c) => {
                            ui.status(Status::Skipping,
                                      format!("{}. It is a dependency of {} packages", &p, c))?;
                        }
                    }
                }
            }
        }
        match execution_strategy {
            ExecutionStrategy::DryRun => {
                ui.end(format!("Would uninstall {} and {} dependencies (Dry run)",
                               &ident, count))?;
            }
            ExecutionStrategy::Run => {
                ui.end(format!("Uninstall of {} and {} dependencies complete",
                               &ident, count))?;
            }
        };
    }
    Ok(())
}

/// Check if we have a launcher/supervisor running out of this habitat root.
/// If the launcher PID file exists then the supervisor is up and running
fn launcher_is_running(fs_root_path: &Path) -> bool {
    let launcher_root = hfs::launcher_root_path(Some(fs_root_path));
    let pid_file_path = launcher_root.join("PID");

    pid_file_path.is_file()
}

async fn supervisor_services() -> Result<Vec<PackageIdent>> {
    if !launcher_is_running(&*FS_ROOT_PATH) {
        return Ok(vec![]);
    }

    let cfg = config::load()?;
    let secret_key = config::ctl_secret_key(&cfg)?;
    let listen_ctl_addr = ListenCtlAddr::default();
    let msg = habitat_sup_protocol::ctl::SvcStatus::default();

    let mut out: Vec<PackageIdent> = vec![];
    let mut response = SrvClient::request(&listen_ctl_addr, &secret_key, msg).await?;
    while let Some(message_result) = response.next().await {
        let reply = message_result?;
        match reply.message_id() {
            "ServiceStatus" => {
                let m = reply.parse::<habitat_sup_protocol::types::ServiceStatus>()
                             .map_err(SrvClientError::Decode)?;
                out.push(m.ident.into());
            }
            "NetOk" => (),
            "NetErr" => {
                let err = reply.parse::<habitat_sup_protocol::net::NetErr>()
                               .map_err(SrvClientError::Decode)?;
                return Err(SrvClientError::from(err).into());
            }
            _ => {
                warn!("Unexpected status message, {:?}", reply);
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Copy)]
enum UninstallSafetyImpl<'a> {
    SkipIfLoaded(&'a [PackageIdent]),
    Force,
}

impl UninstallSafetyImpl<'_> {
    fn should_skip(&self, ident: &PackageIdent) -> bool {
        if let Self::SkipIfLoaded(services) = self {
            services.iter().any(|i| i.satisfies(ident))
        } else {
            false
        }
    }
}

/// Delete a package from disk, depending upon the ExecutionStrategy supplied
///
/// Returns:
///   Ok(true) - package is deleted
///   Ok(false) - package would be deleted but it's a dry run
///   Err(_) -  IO problem deleting package from filesystem
fn maybe_delete<U>(ui: &mut U,
                   fs_root_path: &Path,
                   install: &PackageInstall,
                   strategy: ExecutionStrategy,
                   excludes: &[PackageIdent],
                   safety: UninstallSafetyImpl)
                   -> Result<bool>
    where U: UIWriter
{
    let ident = install.ident();
    let pkg_root_path = hfs::pkg_root_path(Some(fs_root_path));

    let hab = PackageIdent::from_str("core/hab")?;
    if ident.satisfies(&hab) {
        ui.status(Status::Skipping,
                  format!("{}. You can't uninstall core/hab", &ident))?;
        return Ok(false);
    }

    if safety.should_skip(ident) {
        ui.status(Status::Skipping,
                  format!("{}. It is currently loaded by the supervisor", &ident))?;
        return Ok(false);
    }

    // The excludes list could be looser than the fully qualified idents.  E.g. if core/redis is on
    // the exclude list then we should exclude core/redis/1.1.0/20180608091936.  We use the
    // `Identifiable` trait which supplies this logic for PackageIdents
    let should_exclude = excludes.iter().any(|i| i.satisfies(ident));
    if should_exclude {
        ui.status(Status::Skipping,
                  format!("{}. It is on the exclusion list", &ident))?;
        return Ok(false);
    }

    match strategy {
        ExecutionStrategy::DryRun => {
            ui.status(Status::DryRunDeleting, &ident)?;
            Ok(false)
        }
        ExecutionStrategy::Run => {
            ui.status(Status::Deleting, &ident)?;
            let pkg_dir = install.installed_path();
            do_clean_delete(&pkg_root_path, &pkg_dir)
        }
    }
}

/// Delete empty parent directories from a given path. don't traverse above
/// the `pkg_root_path`
fn do_clean_delete(pkg_root_path: &Path, real_install_path: &Path) -> Result<bool> {
    // This match will always return Ok(Path) as the install path is always 4 levels
    // below the pkg_root_path
    match real_install_path.parent() {
        Some(real_install_base) => {
            let temp_install_path = temp_package_directory(real_install_path)?.path()
                                                                              .to_path_buf();
            fs::rename(&real_install_path, &temp_install_path)?;
            fs::remove_dir_all(&temp_install_path)?;

            for p in real_install_base.ancestors() {
                // Let's be safe and not rm below the package directories
                if p == pkg_root_path {
                    break;
                }
                match p.read_dir() {
                    Ok(contents) => {
                        // This will calculate the amount of items in the directory
                        match contents.count() {
                            0 => fs::remove_dir(&p)?,
                            _ => break,
                        }
                    }
                    Err(e) => return Err(Error::HabitatCore(herror::Error::IO(e))),
                }
            }
            Ok(true)
        }
        None => unreachable!("Install path doesn't have a parent"),
    }
}
