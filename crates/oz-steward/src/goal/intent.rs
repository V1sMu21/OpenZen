//! Cheap, deterministic task-intent detection for automatic Goal Mode.
//!
//! Goal Mode is not a manual checkbox: a task that is explicitly open-ended
//! ("until it passes", "keep going", "不完成任务不罢休") or that combines a
//! coding action with a verification loop is promoted automatically. The
//! detector is deliberately conservative; an explicit opt-out always wins.

use serde::{Deserialize, Serialize};

use super::spec::GoalGate;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalIntent {
    pub should_goal: bool,
    pub reason: String,
    pub gate: GoalGate,
}

pub fn detect(message: &str) -> GoalIntent {
    let text = message.trim();
    let lower = text.to_lowercase();
    let opt_out = [
        "不要 goal",
        "不要goal",
        "不要自动",
        "不用 goal",
        "一次性",
        "只回答",
        "只回复",
        "别继续",
        "one-shot",
        "one shot",
        "no goal",
        "don't keep",
        "do not keep",
    ]
    .iter()
    .any(|p| lower.contains(p));
    if opt_out || text.chars().count() < 12 {
        return GoalIntent {
            should_goal: false,
            reason: "explicit opt-out or too short".into(),
            gate: GoalGate::human(),
        };
    }

    let explicit = [
        "goal mode",
        "目标模式",
        "不完成任务不罢休",
        "自动继续",
        "autonomous",
        "keep going until",
        "until it passes",
    ]
    .iter()
    .any(|p| lower.contains(p));
    let cues = [
        "直到",
        "一直",
        "持续",
        "自动",
        "不要停",
        "不完成",
        "保证",
        "跑通",
        "修到",
        "验收",
        "反复",
        "重试",
        "迭代",
        "全程",
        "做完为止",
        "until",
        "keep going",
        "make sure",
        "verify",
        "iterate",
        "retry",
        "end-to-end",
        "e2e",
    ]
    .iter()
    .filter(|p| lower.contains(*p))
    .count();
    let actions = [
        "实现",
        "修复",
        "重构",
        "迁移",
        "添加",
        "开发",
        "测试",
        "部署",
        "implement",
        "fix",
        "refactor",
        "migrate",
        "build",
        "test",
        "deploy",
    ]
    .iter()
    .filter(|p| lower.contains(*p))
    .count();
    let long = text.chars().count() >= 100;
    let should_goal = explicit || cues >= 2 || (long && actions >= 1 && cues >= 1);
    let reason = if explicit {
        "explicit goal-mode phrase"
    } else if cues >= 2 {
        "multiple long-running/verification cues"
    } else if should_goal {
        "long task with an action and a verification cue"
    } else {
        "ordinary task"
    };
    GoalIntent {
        should_goal,
        reason: reason.into(),
        gate: infer_gate(&lower),
    }
}

fn infer_gate(lower: &str) -> GoalGate {
    // Snapshot a concrete verification command when the user names one.
    // Otherwise require human acceptance rather than trusting the model.
    for command in [
        "cargo test",
        "cargo check",
        "cargo build",
        "pytest",
        "npm test",
        "npm run build",
        "bun test",
        "go test",
        "make test",
    ] {
        if lower.contains(command) {
            return GoalGate::Command {
                command: command.into(),
                timeout_secs: 300,
                retries: 0,
            };
        }
    }
    GoalGate::human()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_short_task_does_not_trigger() {
        assert!(!detect("修复登录页").should_goal);
        assert!(!detect("hello").should_goal);
    }

    #[test]
    fn explicit_goal_phrase_triggers() {
        let i = detect("目标模式：把测试修到全绿，不完成任务不罢休");
        assert!(i.should_goal);
        assert!(i.reason.contains("explicit"));
    }

    #[test]
    fn verification_loop_triggers_and_infers_command_gate() {
        let i = detect("实现登录页，反复跑 cargo test 直到全部通过，不要停");
        assert!(i.should_goal);
        assert!(matches!(i.gate, GoalGate::Command { .. }));
    }

    #[test]
    fn opt_out_wins() {
        assert!(!detect("只回答一句话，不要 goal，不要自动继续").should_goal);
    }

    #[test]
    fn long_task_without_cue_is_not_goal() {
        let task = "请实现一个包含很多字段的设置页面，字段包括主题、语言、字体、快捷键和通知开关。";
        assert!(!detect(task).should_goal);
    }
}
