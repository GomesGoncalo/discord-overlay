//! Participant state and animations.

use crate::discord::{self, UserId};

pub struct ParticipantState {
    pub user_id: UserId,
    pub display_name: String,
    pub muted: bool,
    pub deafened: bool,
    pub speaking_until: Option<std::time::Instant>,
    /// 0.0 = invisible (enter start / leave end), 1.0 = fully visible.
    pub anim: f32,
    /// True when this participant is animating out before removal.
    pub leaving: bool,
    /// Speaking ring pulse: animates 0→1 when speaking, 1→0 when silence detected.
    pub speaking_anim: f32,
    /// Accumulated talk time from all completed speaking segments.
    pub talk_time: std::time::Duration,
    /// Instant when the current speaking segment started (None if not speaking).
    pub speaking_started_at: Option<std::time::Instant>,
}

impl ParticipantState {
    /// Total talk time including any currently-active speaking segment.
    pub fn current_talk_secs(&self) -> u64 {
        let base = self.talk_time.as_secs();
        let current = self
            .speaking_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        base + current
    }
}

/// Builder for ParticipantState with sensible defaults.
pub struct ParticipantStateBuilder {
    user_id: UserId,
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
    pub fn new(user_id: impl Into<UserId>, display_name: impl Into<String>) -> Self {
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
            speaking_anim: 0.0,
            talk_time: std::time::Duration::ZERO,
            speaking_started_at: None,
        }
    }
}
