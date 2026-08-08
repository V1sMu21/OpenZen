use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ReflectModule;

/// A single scheduled task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    pub cron_expr: String,
    pub prompt: String,
    pub enabled: bool,
    pub last_run: Option<chrono::DateTime<chrono::Utc>>,
    pub interval_secs: u64,
}

impl ScheduledTask {
    pub fn new(name: &str, prompt: &str, interval_secs: u64) -> Self {
        ScheduledTask {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            cron_expr: String::new(),
            prompt: prompt.to_string(),
            enabled: true,
            last_run: None,
            interval_secs,
        }
    }

    /// Check if this task is due to run.
    pub fn is_due(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        if !self.enabled {
            return false;
        }
        match self.last_run {
            None => true,
            Some(last) => {
                let elapsed = now - last;
                elapsed.num_seconds() as u64 >= self.interval_secs
            }
        }
    }
}

/// Scheduler reflect module — runs tasks on a schedule.
pub struct SchedulerModule {
    tasks_file: PathBuf,
    tasks: Vec<ScheduledTask>,
}

impl SchedulerModule {
    pub fn new(base_dir: &Path) -> Self {
        let tasks_file = base_dir.join("config").join("scheduled_tasks.json");
        let tasks = if tasks_file.exists() {
            std::fs::read_to_string(&tasks_file)
                .ok()
                .and_then(|content| serde_json::from_str(&content).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        SchedulerModule { tasks_file, tasks }
    }

    /// Add a scheduled task.
    pub fn add_task(&mut self, task: ScheduledTask) {
        self.tasks.push(task);
        self.save_tasks();
    }

    /// Remove a task by ID.
    pub fn remove_task(&mut self, id: &str) -> bool {
        let len_before = self.tasks.len();
        self.tasks.retain(|t| t.id != id);
        if self.tasks.len() != len_before {
            self.save_tasks();
            true
        } else {
            false
        }
    }

    /// Record that a task just ran.
    pub fn record_run(&mut self, id: &str) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.last_run = Some(chrono::Utc::now());
            self.save_tasks();
        }
    }

    pub fn tasks(&self) -> &[ScheduledTask] {
        &self.tasks
    }

    fn save_tasks(&self) {
        if let Ok(content) = serde_json::to_string_pretty(&self.tasks) {
            if let Some(parent) = self.tasks_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&self.tasks_file, content);
        }
    }
}

#[async_trait]
impl ReflectModule for SchedulerModule {
    fn name(&self) -> &'static str {
        "scheduler"
    }

    async fn check(&self) -> Option<String> {
        let now = chrono::Utc::now();
        for task in &self.tasks {
            if task.is_due(now) {
                return Some(format!(
                    "[SCHEDULER] Task due: {} — {}",
                    task.name, task.prompt
                ));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduled_task_new() {
        let task = ScheduledTask::new("test-task", "do something", 3600);
        assert_eq!(task.name, "test-task");
        assert!(task.enabled);
        assert!(task.last_run.is_none());
    }

    #[test]
    fn test_task_is_due_never_run() {
        let task = ScheduledTask::new("test", "do it", 3600);
        assert!(task.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_task_not_due_yet() {
        let mut task = ScheduledTask::new("test", "do it", 3600);
        task.last_run = Some(chrono::Utc::now());
        // Just ran, should not be due
        assert!(!task.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_task_due_after_interval() {
        let mut task = ScheduledTask::new("test", "do it", 1);
        let past = chrono::Utc::now() - chrono::Duration::seconds(2);
        task.last_run = Some(past);
        assert!(task.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_task_disabled_not_due() {
        let mut task = ScheduledTask::new("test", "do it", 1);
        task.enabled = false;
        assert!(!task.is_due(chrono::Utc::now()));
    }

    #[test]
    fn test_scheduler_add_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let mut module = SchedulerModule::new(tmp.path());
        assert!(module.tasks().is_empty());

        let task = ScheduledTask::new("test", "prompt", 60);
        let id = task.id.clone();
        module.add_task(task);
        assert_eq!(module.tasks().len(), 1);

        assert!(module.remove_task(&id));
        assert!(module.tasks().is_empty());
    }
}
