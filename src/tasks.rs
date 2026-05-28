use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const TASKS_DIR: &str = ".micos/tasks";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Task {
    pub id: String,
    pub subject: String,
    pub description: String,
    pub active_form: Option<String>,
    pub status: TaskStatus,
    pub blocks: Vec<String>,
    pub blocked_by: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct TaskList {
    root: PathBuf,
    next_id: usize,
    tasks: Vec<Task>,
}

impl TaskList {
    pub fn load_or_init(cwd: &std::path::Path) -> Result<Self> {
        let root = cwd.join(TASKS_DIR);
        std::fs::create_dir_all(&root).context("create tasks directory")?;

        let mut tasks = Vec::new();
        let mut max_id = 0usize;
        if root.exists() {
            for entry in std::fs::read_dir(&root).context("read tasks directory")? {
                let entry = entry.context("read task entry")?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        if let Ok(task) = serde_json::from_str::<Task>(&text) {
                            if let Ok(id) = task.id.parse::<usize>() {
                                max_id = max_id.max(id);
                            }
                            tasks.push(task);
                        }
                    }
                }
            }
        }

        tasks.sort_by(|a, b| {
            a.id.parse::<usize>()
                .unwrap_or(0)
                .cmp(&b.id.parse::<usize>().unwrap_or(0))
        });

        Ok(Self {
            root,
            next_id: max_id + 1,
            tasks,
        })
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn pending_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .count()
    }

    pub fn create(
        &mut self,
        subject: String,
        description: String,
        active_form: Option<String>,
    ) -> Result<Task> {
        let id = self.next_id.to_string();
        self.next_id += 1;
        let task = Task {
            id,
            subject,
            description,
            active_form,
            status: TaskStatus::Pending,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
        };
        self.write_task(&task)?;
        self.tasks.push(task.clone());
        Ok(task)
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn update(
        &mut self,
        id: &str,
        subject: Option<String>,
        description: Option<String>,
        active_form: Option<String>,
        status: Option<TaskStatus>,
    ) -> Result<Task> {
        let task = self
            .tasks
            .iter_mut()
            .find(|t| t.id == id)
            .context("task not found")?;

        if let Some(s) = subject {
            task.subject = s;
        }
        if let Some(d) = description {
            task.description = d;
        }
        if let Some(a) = active_form {
            task.active_form = Some(a);
        }
        if let Some(st) = status {
            task.status = st;
        }

        let task = task.clone();
        self.write_task(&task)?;
        Ok(task)
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        let path = self.task_path(id);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("delete task file {}", path.display()))?;
        }
        self.tasks.retain(|t| t.id != id);
        // Clean up references from other tasks
        let mut to_write = Vec::new();
        for task in &mut self.tasks {
            task.blocks.retain(|bid| bid != id);
            task.blocked_by.retain(|bid| bid != id);
            to_write.push(task.clone());
        }
        for task in &to_write {
            self.write_task(task)?;
        }
        Ok(())
    }

    fn task_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{}.json", id))
    }

    fn write_task(&self, task: &Task) -> Result<()> {
        let path = self.task_path(&task.id);
        let text = serde_json::to_string_pretty(task).context("serialize task")?;
        std::fs::write(&path, text).with_context(|| format!("write task file {}", path.display()))
    }

    pub fn reminder_text(&self) -> Option<String> {
        let active: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| t.status != TaskStatus::Completed)
            .collect();
        if active.is_empty() {
            return None;
        }

        let mut lines = Vec::new();
        for task in &active {
            let status_mark = match task.status {
                TaskStatus::Pending => "[ ]",
                TaskStatus::InProgress => "[>]",
                TaskStatus::Completed => "[x]",
            };
            let mut line = format!("#{} {} {}", task.id, status_mark, task.subject);
            if !task.blocked_by.is_empty() {
                line.push_str(&format!(" (blocked by: {})", task.blocked_by.join(", ")));
            }
            lines.push(line);
        }

        Some(format!(
            "Current tasks:\n{}\n\nUse TaskCreate/TaskUpdate/TaskList/TaskGet to manage tasks.",
            lines.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_lists_tasks() {
        let dir = std::env::temp_dir().join(format!("micos-tasks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut list = TaskList::load_or_init(&dir).unwrap();
        assert_eq!(list.task_count(), 0);

        list.create(
            "fix bug".into(),
            "fix the login bug".into(),
            Some("Fixing bug".into()),
        )
        .unwrap();
        list.create("add test".into(), "add unit test".into(), None)
            .unwrap();

        assert_eq!(list.task_count(), 2);
        assert_eq!(list.pending_count(), 2);

        let reminder = list.reminder_text().unwrap();
        assert!(reminder.contains("fix bug"));
        assert!(reminder.contains("add test"));
    }

    #[test]
    fn updates_and_deletes_tasks() {
        let dir = std::env::temp_dir().join(format!("micos-tasks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut list = TaskList::load_or_init(&dir).unwrap();

        let task = list.create("fix bug".into(), "desc".into(), None).unwrap();
        assert_eq!(task.status, TaskStatus::Pending);

        list.update(&task.id, None, None, None, Some(TaskStatus::InProgress))
            .unwrap();
        assert_eq!(list.get(&task.id).unwrap().status, TaskStatus::InProgress);

        list.update(&task.id, None, None, None, Some(TaskStatus::Completed))
            .unwrap();
        assert_eq!(list.get(&task.id).unwrap().status, TaskStatus::Completed);

        list.delete(&task.id).unwrap();
        assert_eq!(list.task_count(), 0);
    }
}
