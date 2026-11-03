//! One SSH transport reconnects once. Each live folder then resolves its own
//! immutable context on the replacement, retaining its buffers and recovery.
use super::*;
use crate::app_remote_connection::Phase;

impl App {
    pub(crate) fn show_workspace_connection_sign_in(&mut self) {
        let Some(source) = self.remote_workspace.as_ref() else {
            return;
        };
        if let Some(parked) = self.workspaces.parked.iter_mut().find(|parked| {
            parked
                .app
                .remote_workspace
                .as_ref()
                .is_some_and(|remote| remote.same_family(source))
                && parked.app.remote_connection.phase == Phase::Connecting
        }) {
            parked.app.remote_connection.details = true;
            self.workspaces.request = Some(Request::Switch(parked.id));
        } else if self.remote_connection.phase == Phase::Connecting {
            self.remote_connection.details = true;
        }
    }
    pub(crate) fn coordinate_workspace_connections(&mut self) {
        // A successful handshake may be followed by a missing owner folder.
        // Take ownership before lending that transport to other workspaces:
        // closing/stopping the failed owner must not disconnect its siblings.
        let mut recovered = Vec::new();
        if let (Some(source), Some(candidate)) = (
            self.remote_workspace.clone(),
            self.remote_connection.take_shared_candidate(),
        ) {
            recovered.push((source, candidate));
        }
        for parked in &mut self.workspaces.parked {
            if let (Some(source), Some(candidate)) = (
                parked.app.remote_workspace.clone(),
                parked.app.remote_connection.take_shared_candidate(),
            ) {
                recovered.push((source, candidate));
            }
        }
        let members: Vec<_> = std::iter::once((self.workspaces.active, &*self))
            .chain(
                self.workspaces
                    .parked
                    .iter()
                    .map(|parked| (parked.id, &*parked.app)),
            )
            .filter_map(|(id, app)| {
                app.remote_workspace.clone().map(|remote| {
                    (
                        id,
                        remote,
                        app.remote_connection.phase.clone(),
                        app.remote_connection.shared_attempt.clone(),
                        app.exit_keep_unsaved_quit
                            || app.exit_discard_quit
                            || app.exit_save_and_quit
                            || app.shutdown_anim_active,
                    )
                })
            })
            .collect();
        let mut handled = Vec::new();
        for (_, source, _, _, _) in &members {
            if handled
                .iter()
                .any(|other: &crate::remote::workspace::Workspace| other.same_family(source))
            {
                continue;
            }
            handled.push(source.clone());
            let group: Vec<_> = members
                .iter()
                .filter(|(_, remote, _, _, _)| source.same_family(remote))
                .collect();
            let connected = group
                .iter()
                .find(|(_, remote, _, _, exiting)| remote.client.connected() && !exiting)
                .map(|(_, remote, _, _, _)| remote.clone())
                .or_else(|| {
                    recovered
                        .iter()
                        .find(|(owner, _)| owner.same_family(source))
                        .map(|(_, remote)| remote.clone())
                });
            // Reserve one owner even while healthy. The transport may close
            // after this snapshot and before each workspace polls its loss;
            // clearing all follower flags would let every folder launch SSH.
            let leader = group
                .iter()
                .find(|(_, remote, phase, _, exiting)| {
                    !exiting
                        && remote.reconnect_allowed()
                        && matches!(
                            phase,
                            Phase::Starting | Phase::Connecting | Phase::Resolving
                        )
                })
                .or_else(|| {
                    group.iter().find(|(_, remote, phase, _, exiting)| {
                        !exiting && remote.reconnect_allowed() && *phase != Phase::Stopped
                    })
                })
                .map(|(id, _, _, _, _)| *id);
            for (id, remote, phase, attempted, exiting) in group {
                let app = if *id == self.workspaces.active {
                    &mut *self
                } else {
                    &mut self
                        .workspaces
                        .parked
                        .iter_mut()
                        .find(|parked| parked.id == *id)
                        .unwrap()
                        .app
                };
                app.remote_connection.shared_follower = leader.is_some_and(|leader| leader != *id);
                if let Some(connected) = &connected
                    && !exiting
                    && !remote.client.connected()
                    && !attempted.as_ref().is_some_and(|client| {
                        client.same_transport(&connected.client)
                            && matches!(phase, Phase::Resolving | Phase::Stopped)
                    })
                {
                    app.begin_shared_remote_resume(connected.clone());
                }
            }
        }
    }

    pub(crate) fn stop_remote_reconnect(&mut self) {
        let Some(source) = self.remote_workspace.clone() else {
            return;
        };
        let connected = std::iter::once(&*self)
            .chain(self.workspaces.parked.iter().map(|parked| &*parked.app))
            .filter_map(|app| app.remote_workspace.as_ref())
            .find(|remote| remote.same_family(&source) && remote.client.connected())
            .map(|remote| remote.client.clone());
        self.remote_connection.shared_follower = true;
        self.poll_remote_connection(Instant::now());
        self.stop_remote_reconnect_local();
        if !source.client.connected() {
            self.remote_connection.shared_attempt = connected.clone();
        }
        for parked in &mut self.workspaces.parked {
            if parked
                .app
                .remote_workspace
                .as_ref()
                .is_some_and(|remote| remote.same_family(&source))
            {
                parked.app.remote_connection.shared_follower = true;
                parked.app.poll_remote_connection(Instant::now());
                parked.app.stop_remote_reconnect_local();
                if parked
                    .app
                    .remote_workspace
                    .as_ref()
                    .is_some_and(|remote| !remote.client.connected())
                {
                    parked.app.remote_connection.shared_attempt = connected.clone();
                }
            }
        }
    }
}
