// SPDX-License-Identifier: AGPL-3.0-only

use cosmic::iced::window::Id as WindowId;

#[derive(Clone, Debug)]
struct PopupSession {
    group: String,
    window_id: WindowId,
    generation: u64,
}

#[derive(Clone, Debug, Default)]
enum PopupState {
    #[default]
    Closed,
    HoverOpen(PopupSession),
    Pinned(PopupSession),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenPlan {
    None,
    Create {
        window_id: WindowId,
        group: String,
    },
    Replace {
        old_window_id: WindowId,
        window_id: WindowId,
        group: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CloseGuard {
    generation: u64,
    token: u64,
}

#[derive(Debug, Default)]
pub(crate) struct PopupFsm {
    state: PopupState,
    pointer_group: Option<String>,
    pointer_in_popup: bool,
    next_generation: u64,
    close_token: u64,
}

impl PopupFsm {
    pub(crate) fn active_group(&self) -> Option<&str> {
        self.session().map(|session| session.group.as_str())
    }

    pub(crate) fn window_id(&self) -> Option<WindowId> {
        self.session().map(|session| session.window_id)
    }

    pub(crate) fn is_open(&self) -> bool {
        self.session().is_some()
    }

    pub(crate) fn is_pinned(&self) -> bool {
        matches!(self.state, PopupState::Pinned(_))
    }

    pub(crate) fn group_enter(&mut self, group: String) {
        if self.active_group().is_some_and(|active| active != group) {
            // Do not cancel the previous group's pending close merely because the
            // pointer crossed directly onto another app icon. Live cross-group
            // replacement used to destroy/create popup surfaces in one event-loop
            // turn and proved unstable on real COSMIC sessions.
            self.pointer_group = None;
            return;
        }
        self.pointer_group = Some(group);
        self.invalidate_close();
    }

    pub(crate) fn group_exit(&mut self, group: &str) {
        if self.pointer_group.as_deref() == Some(group) {
            self.pointer_group = None;
        }
    }

    pub(crate) fn popup_enter(&mut self) {
        self.pointer_in_popup = true;
        self.invalidate_close();
    }

    pub(crate) fn popup_exit(&mut self) {
        self.pointer_in_popup = false;
    }

    pub(crate) fn request_open(
        &mut self,
        group: String,
        pinned: bool,
        new_window_id: WindowId,
    ) -> OpenPlan {
        let previous = std::mem::take(&mut self.state);

        match previous {
            PopupState::Closed => {
                self.invalidate_close();
                let session = self.new_session(group.clone(), new_window_id);
                self.state = if pinned {
                    PopupState::Pinned(session)
                } else {
                    PopupState::HoverOpen(session)
                };
                OpenPlan::Create {
                    window_id: new_window_id,
                    group,
                }
            }
            PopupState::HoverOpen(session) => {
                if session.group == group {
                    self.invalidate_close();
                    self.state = if pinned {
                        PopupState::Pinned(session)
                    } else {
                        PopupState::HoverOpen(session)
                    };
                    OpenPlan::None
                } else {
                    // Stability-first rule: never live-replace one hover popup with
                    // another. Let the current popup finish its normal close path,
                    // then the new group may open on a fresh enter/click.
                    self.state = PopupState::HoverOpen(session);
                    OpenPlan::None
                }
            }
            PopupState::Pinned(session) => {
                if session.group == group {
                    self.invalidate_close();
                }
                // A pinned surface is never replaced from another group while it is
                // alive. This deliberately leaves OpenPlan::Replace unreachable in
                // the runtime path until we implement compositor-acknowledged
                // two-phase replacement.
                self.state = PopupState::Pinned(session);
                OpenPlan::None
            }
        }
    }

    pub(crate) fn schedule_close(&mut self) -> Option<CloseGuard> {
        let generation = match &self.state {
            PopupState::HoverOpen(session) => session.generation,
            PopupState::Closed | PopupState::Pinned(_) => return None,
        };
        self.close_token = self.close_token.wrapping_add(1);
        Some(CloseGuard {
            generation,
            token: self.close_token,
        })
    }

    pub(crate) fn should_close(&self, guard: CloseGuard) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        matches!(self.state, PopupState::HoverOpen(_))
            && session.generation == guard.generation
            && self.close_token == guard.token
            && self.pointer_group.is_none()
            && !self.pointer_in_popup
    }

    pub(crate) fn close_current(&mut self) -> Option<WindowId> {
        let id = self.window_id();
        self.state = PopupState::Closed;
        self.pointer_group = None;
        self.pointer_in_popup = false;
        self.invalidate_close();
        id
    }

    pub(crate) fn compositor_closed(&mut self, id: WindowId) -> bool {
        if self.window_id() != Some(id) {
            return false;
        }
        self.state = PopupState::Closed;
        self.pointer_group = None;
        self.pointer_in_popup = false;
        self.invalidate_close();
        true
    }

    fn session(&self) -> Option<&PopupSession> {
        match &self.state {
            PopupState::Closed => None,
            PopupState::HoverOpen(session) | PopupState::Pinned(session) => Some(session),
        }
    }

    fn new_session(&mut self, group: String, window_id: WindowId) -> PopupSession {
        self.next_generation = self.next_generation.wrapping_add(1);
        if self.next_generation == 0 {
            self.next_generation = 1;
        }
        PopupSession {
            group,
            window_id,
            generation: self.next_generation,
        }
    }

    fn invalidate_close(&mut self) {
        self.close_token = self.close_token.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_open_from_closed_creates_surface() {
        let mut fsm = PopupFsm::default();
        let id = WindowId::unique();
        fsm.group_enter("brave".into());
        let plan = fsm.request_open("brave".into(), false, id);
        assert_eq!(
            plan,
            OpenPlan::Create {
                window_id: id,
                group: "brave".into()
            }
        );
        assert_eq!(fsm.active_group(), Some("brave"));
        assert!(!fsm.is_pinned());
    }

    #[test]
    fn pinning_same_group_reuses_surface() {
        let mut fsm = PopupFsm::default();
        let id = WindowId::unique();
        fsm.request_open("brave".into(), false, id);
        let generation = fsm.session().unwrap().generation;
        assert_eq!(
            fsm.request_open("brave".into(), true, WindowId::unique()),
            OpenPlan::None
        );
        assert_eq!(fsm.window_id(), Some(id));
        assert_eq!(fsm.session().unwrap().generation, generation);
        assert!(fsm.is_pinned());
    }

    #[test]
    fn cross_group_hover_never_live_replaces_surface() {
        let mut fsm = PopupFsm::default();
        let old = WindowId::unique();
        fsm.request_open("brave".into(), false, old);
        let generation = fsm.session().unwrap().generation;
        let plan = fsm.request_open("spotify".into(), false, WindowId::unique());
        assert_eq!(plan, OpenPlan::None);
        assert_eq!(fsm.window_id(), Some(old));
        assert_eq!(fsm.active_group(), Some("brave"));
        assert_eq!(fsm.session().unwrap().generation, generation);
    }

    #[test]
    fn crossing_to_another_icon_does_not_cancel_old_close_guard() {
        let mut fsm = PopupFsm::default();
        fsm.group_enter("brave".into());
        fsm.request_open("brave".into(), false, WindowId::unique());
        fsm.group_exit("brave");
        let guard = fsm.schedule_close().unwrap();
        fsm.group_enter("spotify".into());
        assert!(fsm.should_close(guard));
    }

    #[test]
    fn stale_close_guard_cannot_close_after_reenter_same_group() {
        let mut fsm = PopupFsm::default();
        fsm.request_open("brave".into(), false, WindowId::unique());
        fsm.group_exit("brave");
        let guard = fsm.schedule_close().unwrap();
        fsm.group_enter("brave".into());
        assert!(!fsm.should_close(guard));
    }

    #[test]
    fn stale_compositor_close_is_ignored() {
        let mut fsm = PopupFsm::default();
        let current = WindowId::unique();
        fsm.request_open("brave".into(), false, current);
        assert!(!fsm.compositor_closed(WindowId::unique()));
        assert_eq!(fsm.window_id(), Some(current));
    }

    #[test]
    fn current_compositor_close_clears_surface_and_pointer_state() {
        let mut fsm = PopupFsm::default();
        let id = WindowId::unique();
        fsm.group_enter("brave".into());
        fsm.request_open("brave".into(), false, id);
        assert!(fsm.compositor_closed(id));
        assert!(!fsm.is_open());
        assert_eq!(fsm.pointer_group, None);
    }

    #[test]
    fn pinned_popup_never_schedules_hover_close() {
        let mut fsm = PopupFsm::default();
        fsm.request_open("brave".into(), true, WindowId::unique());
        fsm.group_exit("brave");
        assert_eq!(fsm.schedule_close(), None);
    }
}
