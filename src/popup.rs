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
        self.invalidate_close();
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
                    let session = self.new_session(group.clone(), new_window_id);
                    self.state = if pinned {
                        PopupState::Pinned(session)
                    } else {
                        PopupState::HoverOpen(session)
                    };
                    OpenPlan::Replace {
                        old_window_id,
                        window_id: new_window_id,
                        group,
                    }
                }
            }
            PopupState::Pinned(session) => {
                if !pinned {
                    self.state = PopupState::Pinned(session);
                    OpenPlan::None
                } else if session.group == group {
                    self.state = PopupState::Pinned(session);
                    OpenPlan::None
                } else {
                    let old_window_id = session.window_id;
                    let session = self.new_session(group.clone(), new_window_id);
                    self.state = PopupState::Pinned(session);
                    OpenPlan::Replace {
                        old_window_id,
                        window_id: new_window_id,
                        group,
                    }
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
    fn switching_hover_group_replaces_and_reanchors_surface() {
        let mut fsm = PopupFsm::default();
        let old = WindowId::unique();
        let new = WindowId::unique();
        fsm.request_open("brave".into(), false, old);
        let old_generation = fsm.session().unwrap().generation;
        let plan = fsm.request_open("firefox".into(), false, new);
        assert_eq!(
            plan,
            OpenPlan::Replace {
                old_window_id: old,
                window_id: new,
                group: "firefox".into()
            }
        );
        assert_eq!(fsm.active_group(), Some("firefox"));
        assert!(fsm.session().unwrap().generation > old_generation);
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
    fn stale_close_guard_cannot_close_new_generation() {
        let mut fsm = PopupFsm::default();
        fsm.request_open("brave".into(), false, WindowId::unique());
        let guard = fsm.schedule_close().unwrap();
        fsm.request_open("firefox".into(), false, WindowId::unique());
        assert!(!fsm.should_close(guard));
    }

    #[test]
    fn stale_compositor_close_is_ignored() {
        let mut fsm = PopupFsm::default();
        let old = WindowId::unique();
        let new = WindowId::unique();
        fsm.request_open("brave".into(), false, old);
        fsm.request_open("firefox".into(), false, new);
        assert!(!fsm.compositor_closed(old));
        assert_eq!(fsm.window_id(), Some(new));
    }

    #[test]
    fn current_compositor_close_clears_surface() {
        let mut fsm = PopupFsm::default();
        let id = WindowId::unique();
        fsm.request_open("brave".into(), false, id);
        assert!(fsm.compositor_closed(id));
        assert!(!fsm.is_open());
    }

    #[test]
    fn pinned_popup_never_schedules_hover_close() {
        let mut fsm = PopupFsm::default();
        fsm.request_open("brave".into(), true, WindowId::unique());
        fsm.group_exit("brave");
        assert_eq!(fsm.schedule_close(), None);
    }
}
