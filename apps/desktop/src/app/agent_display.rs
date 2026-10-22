use super::ai_runtime_status::AgentLifecycleState;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentBadgeShape {
    Circle,
    Square,
    Ring,
    Question,
    Hidden,
}

fn agent_badge_style(state: AgentLifecycleState) -> (AgentBadgeShape, u32) {
    match state {
        AgentLifecycleState::Working => (AgentBadgeShape::Circle, 0x1D4ED8),
        AgentLifecycleState::NeedsInput => (AgentBadgeShape::Square, 0xB45309),
        AgentLifecycleState::Idle => (AgentBadgeShape::Ring, theme::TEXT_MUTED),
        AgentLifecycleState::Unknown => (AgentBadgeShape::Question, theme::TEXT_MUTED),
        AgentLifecycleState::Absent => (AgentBadgeShape::Hidden, theme::TEXT_DIM),
    }
}

pub(in crate::app) fn agent_lifecycle_color(state: AgentLifecycleState) -> gpui::Hsla {
    color(agent_badge_style(state).1)
}

/// Static, opaque geometry carries meaning even without color or animation.
pub(in crate::app) fn agent_lifecycle_status_dot(state: AgentLifecycleState) -> AnyElement {
    let (shape, tint) = agent_badge_style(state);
    let base = div()
        .flex_none()
        .size(px(14.0))
        .border_2()
        .border_color(color(theme::TEXT))
        .bg(color(tint));
    let inner = match shape {
        AgentBadgeShape::Circle => base.rounded_full().into_any_element(),
        AgentBadgeShape::Square => base.rounded(px(2.0)).into_any_element(),
        AgentBadgeShape::Ring => base
            .rounded_full()
            .bg(color(theme::BG_COLUMN))
            .border_color(color(tint))
            .into_any_element(),
        AgentBadgeShape::Question => Icon::new(HeroIconName::QuestionMarkCircle)
            .size(px(14.0))
            .text_color(color(tint))
            .into_any_element(),
        AgentBadgeShape::Hidden => return div().into_any_element(),
    };
    div()
        .flex_none()
        .size(px(18.0))
        .p(px(2.0))
        .rounded(px(3.0))
        .bg(color(theme::BG_COLUMN))
        .child(inner)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_activity_static_shapes_distinguish_work_input_and_idle_without_color() {
        use AgentLifecycleState::*;
        assert_eq!(agent_badge_style(NeedsInput).0, AgentBadgeShape::Square);
        assert_ne!(
            agent_badge_style(Working).0,
            agent_badge_style(NeedsInput).0
        );
        assert_ne!(agent_badge_style(Idle).0, agent_badge_style(Working).0);
        assert_ne!(agent_badge_style(Unknown).0, agent_badge_style(Idle).0);
    }
}
