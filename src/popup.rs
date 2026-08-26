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
struct PendingOpen {
    group: String,
    pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenPlan {
    None,
    Create { window_id: WindowId, group: String },
    CloseForSwitch { old_window_id: WindowId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CloseOutcome {
    Ignored,
    Closed,
    OpenPending { group: String, pinned: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CloseGuard {
    generation: u64,
    token: u64,
}

#[derive(Debug, Default)]
pub(crate) struct PopupFsm {
    state: PopupState,
    pending_open: Option<PendingOpen>,
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

    pub(crate) fn is_hover_open(&self) -> bool {
        matches!(self.state, PopupState::HoverOpen(_))
    }

    pub(crate) fn group_enter(&mut self, group: String) {
        self.pointer_in_popup = false;
        self.pointer_group = Some(group);
        self.invalidate_close();
    }

    pub(crate) fn group_exit(&mut self, group: &str) {
        if self.pointer_group.as_deref() == Some(group) {
            self.pointer_group = None;
        }

        if self
            .pending_open
            .as_ref()
            .is_some_and(|pending| !pending.pinned && pending.group == group)
        {
            self.pending_open = None;
        }
    }

    pub(crate) fn popup_enter(&mut self) {
        self.pointer_group = None;
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
        self.invalidate_close();

        if self.pending_open.is_some() {
            self.pending_open = Some(PendingOpen { group, pinned });
            return OpenPlan::None;
        }

        let previous = std::mem::take(&mut self.state);

        match previous {
            PopupState::Closed => {
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
                    self.state = if pinned {
                        PopupState::Pinned(session)
                    } else {
                        PopupState::HoverOpen(session)
                    };
                    OpenPlan::None
                } else {
                    let old_window_id = session.window_id;
                    self.state = PopupState::HoverOpen(session);
                    self.pending_open = Some(PendingOpen { group, pinned });
                    OpenPlan::CloseForSwitch { old_window_id }
                }
            }
            PopupState::Pinned(session) => {
                if !pinned || session.group == group {
                    self.state = PopupState::Pinned(session);
                    OpenPlan::None
                } else {
                    let old_window_id = session.window_id;
                    self.state = PopupState::Pinned(session);
                    self.pending_open = Some(PendingOpen { group, pinned });
                    OpenPlan::CloseForSwitch { old_window_id }
                }
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
            && self.pending_open.is_none()
            && session.generation == guard.generation
            && self.close_token == guard.token
            && self.pointer_group.is_none()
            && !self.pointer_in_popup
    }

    pub(crate) fn close_current(&mut self) -> Option<WindowId> {
        let id = self.window_id();
        self.state = PopupState::Closed;
        self.pending_open = None;
        self.pointer_group = None;
        self.pointer_in_popup = false;
        self.invalidate_close();
        id
    }

    pub(crate) fn compositor_closed(&mut self, id: WindowId) -> CloseOutcome {
        if self.window_id() != Some(id) {
            return CloseOutcome::Ignored;
        }

        self.state = PopupState::Closed;
        self.pointer_in_popup = false;
        self.invalidate_close();

        let Some(pending) = self.pending_open.take() else {
            self.pointer_group = None;
            return CloseOutcome::Closed;
        };

        if pending.pinned || self.pointer_group.as_deref() == Some(pending.group.as_str()) {
            CloseOutcome::OpenPending {
                group: pending.group,
                pinned: pending.pinned,
            }
        } else {
            self.pointer_group = None;
            CloseOutcome::Closed
        }
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
    fn closed_state_is_not_pinned_or_open() {
        let fsm = PopupFsm::default();
        assert!(!fsm.is_pinned());
        assert!(!fsm.is_open());
        assert!(!fsm.is_hover_open());
    }

    #[test]
    fn explicit_hover_request_keeps_internal_state_consistent() {
        let mut fsm = PopupFsm::default();
        let id = WindowId::unique();
        let plan = fsm.request_open("brave".into(), false, id);
        assert_eq!(
            plan,
            OpenPlan::Create {
                window_id: id,
                group: "brave".into()
            }
        );
        assert_eq!(fsm.active_group(), Some("brave"));
        assert!(fsm.is_hover_open());
    }

    #[test]
    fn click_open_is_pinned() {
        let mut fsm = PopupFsm::default();
        let id = WindowId::unique();
        assert!(matches!(
            fsm.request_open("brave".into(), true, id),
            OpenPlan::Create { .. }
        ));
        assert!(fsm.is_pinned());
        assert_eq!(fsm.window_id(), Some(id));
    }

    #[test]
    fn switching_group_waits_for_compositor_close() {
        let mut fsm = PopupFsm::default();
        let old = WindowId::unique();
        fsm.request_open("brave".into(), true, old);
        assert_eq!(
            fsm.request_open("spotify".into(), true, WindowId::unique()),
            OpenPlan::CloseForSwitch { old_window_id: old }
        );
        assert_eq!(
            fsm.compositor_closed(old),
            CloseOutcome::OpenPending {
                group: "spotify".into(),
                pinned: true
            }
        );
    }

    #[test]
    fn pending_hover_switch_tracks_latest_target_without_second_destroy() {
        let mut fsm = PopupFsm::default();
        let old = WindowId::unique();
        fsm.group_enter("brave".into());
        fsm.request_open("brave".into(), false, old);
        fsm.group_enter("spotify".into());
        assert!(matches!(
            fsm.request_open("spotify".into(), false, WindowId::unique()),
            OpenPlan::CloseForSwitch { .. }
        ));

        fsm.group_enter("firefox".into());
        assert_eq!(
            fsm.request_open("firefox".into(), false, WindowId::unique()),
            OpenPlan::None
        );
        assert_eq!(
            fsm.compositor_closed(old),
            CloseOutcome::OpenPending {
                group: "firefox".into(),
                pinned: false
            }
        );
    }

    #[test]
    fn leaving_pending_hover_target_cancels_reopen() {
        let mut fsm = PopupFsm::default();
        let old = WindowId::unique();
        fsm.group_enter("brave".into());
        fsm.request_open("brave".into(), false, old);
        fsm.group_enter("spotify".into());
        fsm.request_open("spotify".into(), false, WindowId::unique());
        fsm.group_exit("spotify");

        assert_eq!(fsm.compositor_closed(old), CloseOutcome::Closed);
        assert!(!fsm.is_open());
    }

    #[test]
    fn popup_enter_repairs_missing_group_exit() {
        let mut fsm = PopupFsm::default();
        fsm.group_enter("brave".into());
        fsm.request_open("brave".into(), false, WindowId::unique());
        fsm.popup_enter();
        assert_eq!(fsm.pointer_group, None);
        assert!(fsm.pointer_in_popup);

        fsm.popup_exit();
        let guard = fsm.schedule_close().unwrap();
        assert!(fsm.should_close(guard));
    }

    #[test]
    fn stale_close_guard_cannot_close_after_reenter() {
        let mut fsm = PopupFsm::default();
        fsm.request_open("brave".into(), false, WindowId::unique());
        fsm.group_exit("brave");
        let guard = fsm.schedule_close().unwrap();
        fsm.popup_enter();
        assert!(!fsm.should_close(guard));
    }

    #[test]
    fn stale_compositor_close_is_ignored() {
        let mut fsm = PopupFsm::default();
        let current = WindowId::unique();
        fsm.request_open("brave".into(), true, current);
        assert_eq!(
            fsm.compositor_closed(WindowId::unique()),
            CloseOutcome::Ignored
        );
        assert_eq!(fsm.window_id(), Some(current));
    }

    #[test]
    fn close_current_cancels_pending_switch() {
        let mut fsm = PopupFsm::default();
        let old = WindowId::unique();
        fsm.request_open("brave".into(), true, old);
        fsm.request_open("spotify".into(), true, WindowId::unique());
        assert_eq!(fsm.close_current(), Some(old));
        assert_eq!(fsm.compositor_closed(old), CloseOutcome::Ignored);
    }

    #[test]
    fn pinned_popup_never_schedules_hover_close() {
        let mut fsm = PopupFsm::default();
        fsm.request_open("brave".into(), true, WindowId::unique());
        fsm.group_exit("brave");
        assert_eq!(fsm.schedule_close(), None);
    }
}