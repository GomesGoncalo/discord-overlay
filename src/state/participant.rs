//! Participant state and animations.

use crate::discord;

pub struct ParticipantState {
    pub user_id: String,
    pub display_name: String,
    pub muted: bool,
    pub deafened: bool,
    pub speaking_until: Option<std::time::Instant>,
    /// 0.0 = invisible (enter start / leave end), 1.0 = fully visible.
    pub anim: f32,
    /// True when this participant is animating out before removal.
    pub leaving: bool,
}

/// Builder for ParticipantState with sensible defaults.
pub struct ParticipantStateBuilder {
    user_id: String,
    display_name: String,
    muted: bool,
    deafened: bool,
    speaking_until: Option<std::time::Instant>,
    anim: f32,
    leaving: bool,
}

impl ParticipantStateBuilder {
    /// Create a new builder from a discord participant.
    pub fn from_discord(p: &discord::Participant) -> Self {
        Self {
            user_id: p.user_id.clone(),
            display_name: p.nick.clone().unwrap_or_else(|| p.username.clone()),
            muted: p.muted,
            deafened: p.deafened,
            speaking_until: None,
            anim: 1.0, // start visible by default
            leaving: false,
        }
    }

    /// Create a new builder with minimal required fields.
    #[allow(dead_code)]
    pub fn new(user_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            display_name: display_name.into(),
            muted: false,
            deafened: false,
            speaking_until: None,
            anim: 1.0,
            leaving: false,
        }
    }

    pub fn anim(mut self, anim: f32) -> Self {
        self.anim = anim;
        self
    }

    #[allow(dead_code)]
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    #[allow(dead_code)]
    pub fn deafened(mut self, deafened: bool) -> Self {
        self.deafened = deafened;
        self
    }

    #[allow(dead_code)]
    pub fn leaving(mut self, leaving: bool) -> Self {
        self.leaving = leaving;
        self
    }

    pub fn build(self) -> ParticipantState {
        ParticipantState {
            user_id: self.user_id,
            display_name: self.display_name,
            muted: self.muted,
            deafened: self.deafened,
            speaking_until: self.speaking_until,
            anim: self.anim,
            leaving: self.leaving,
        }
    }
}
