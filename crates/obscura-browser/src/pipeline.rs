use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A browsing-context pipeline representing one frame/tab, analogous to
/// servo-constellation's pipeline concept.
pub struct Pipeline {
    pub id: u32,
    pub url: String,
    pub title: String,
    pub parent_id: Option<u32>,
    pub child_ids: Vec<u32>,
    pub state: PipelineState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PipelineState {
    Loading,
    Interactive,
    Complete,
    Error(String),
}

impl Pipeline {
    /// Whether this pipeline is in a terminal/error state.
    pub fn is_error(&self) -> bool {
        matches!(self.state, PipelineState::Error(_))
    }

    /// Whether the pipeline has finished loading (interactive or complete).
    pub fn is_ready(&self) -> bool {
        matches!(self.state, PipelineState::Interactive | PipelineState::Complete)
    }
}

/// The constellation manages all pipelines (browsing contexts), analogous to
/// servo-constellation. In obscura this is a lightweight in-process
/// coordinator rather than a set of OS processes.
pub struct Constellation {
    pipelines: RwLock<HashMap<u32, Arc<RwLock<Pipeline>>>>,
    next_id: RwLock<u32>,
}

impl Constellation {
    pub fn new() -> Self {
        Self {
            pipelines: RwLock::new(HashMap::new()),
            next_id: RwLock::new(1),
        }
    }

    pub async fn create_pipeline(&self, url: &str, parent_id: Option<u32>) -> u32 {
        let mut next = self.next_id.write().await;
        let id = *next;
        *next += 1;

        let pipeline = Pipeline {
            id,
            url: url.to_string(),
            title: String::new(),
            parent_id,
            child_ids: Vec::new(),
            state: PipelineState::Loading,
        };

        let mut pipelines = self.pipelines.write().await;
        pipelines.insert(id, Arc::new(RwLock::new(pipeline)));

        if let Some(parent) = parent_id {
            if let Some(parent_pipeline) = pipelines.get(&parent) {
                let mut parent = parent_pipeline.write().await;
                parent.child_ids.push(id);
            }
        }

        id
    }

    pub async fn navigate(&self, id: u32, url: &str) -> bool {
        let pipelines = self.pipelines.read().await;
        if let Some(pipeline) = pipelines.get(&id) {
            let mut p = pipeline.write().await;
            p.url = url.to_string();
            p.state = PipelineState::Loading;
            true
        } else {
            false
        }
    }

    pub async fn get_pipeline(&self, id: u32) -> Option<Arc<RwLock<Pipeline>>> {
        let pipelines = self.pipelines.read().await;
        pipelines.get(&id).cloned()
    }

    pub async fn set_state(&self, id: u32, state: PipelineState) -> bool {
        let pipelines = self.pipelines.read().await;
        if let Some(pipeline) = pipelines.get(&id) {
            let mut p = pipeline.write().await;
            p.state = state;
            true
        } else {
            false
        }
    }

    pub async fn set_title(&self, id: u32, title: &str) -> bool {
        let pipelines = self.pipelines.read().await;
        if let Some(pipeline) = pipelines.get(&id) {
            let mut p = pipeline.write().await;
            p.title = title.to_string();
            true
        } else {
            false
        }
    }

    /// List all live pipeline ids in creation order.
    pub async fn list_pipelines(&self) -> Vec<u32> {
        let pipelines = self.pipelines.read().await;
        let mut ids: Vec<u32> = pipelines.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Return the ids of every child pipeline of `parent_id`.
    pub async fn children_of(&self, parent_id: u32) -> Vec<u32> {
        let pipelines = self.pipelines.read().await;
        pipelines
            .get(&parent_id)
            .map(|p| p.try_read().map(|p| p.child_ids.clone()).unwrap_or_default())
            .unwrap_or_default()
    }

    /// Find the top-level (root) pipeline for a given id by walking up parents.
    pub async fn root_of(&self, id: u32) -> Option<u32> {
        let mut current = id;
        let pipelines = self.pipelines.read().await;
        loop {
            let parent = pipelines.get(&current).and_then(|p| {
                p.try_read().ok().and_then(|p| p.parent_id)
            });
            match parent {
                Some(pid) => current = pid,
                None => return Some(current),
            }
        }
    }

    pub async fn close_pipeline(&self, id: u32) {
        let mut pipelines = self.pipelines.write().await;
        // Remove `id` from its parent's child list first.
        if let Some(parent_id) = pipelines
            .get(&id)
            .and_then(|p| p.try_read().ok().and_then(|p| p.parent_id))
        {
            if let Some(parent) = pipelines.get(&parent_id) {
                if let Ok(mut parent) = parent.try_write() {
                    parent.child_ids.retain(|&c| c != id);
                }
            }
        }
        // Remove children recursively.
        let children: Vec<u32> = pipelines
            .get(&id)
            .map(|p| p.try_read().map(|p| p.child_ids.clone()).unwrap_or_default())
            .unwrap_or_default();
        for child in children {
            pipelines.remove(&child);
        }
        pipelines.remove(&id);
    }

    /// Number of live pipelines.
    pub async fn len(&self) -> usize {
        self.pipelines.read().await.len()
    }
}

impl Default for Constellation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_get_pipeline() {
        let c = Constellation::new();
        let id = c.create_pipeline("https://example.com", None).await;
        let p = c.get_pipeline(id).await.unwrap();
        assert_eq!(p.read().await.url, "https://example.com");
        assert_eq!(p.read().await.state, PipelineState::Loading);
    }

    #[tokio::test]
    async fn parent_child_relationship() {
        let c = Constellation::new();
        let parent = c.create_pipeline("https://a.com", None).await;
        let child = c.create_pipeline("https://b.com", Some(parent)).await;
        assert_eq!(c.children_of(parent).await, vec![child]);
        assert_eq!(c.root_of(child).await, Some(parent));
    }

    #[tokio::test]
    async fn state_and_title_updates() {
        let c = Constellation::new();
        let id = c.create_pipeline("https://a.com", None).await;
        assert!(c.set_state(id, PipelineState::Complete).await);
        assert!(c.set_title(id, "My Page").await);
        let p = c.get_pipeline(id).await.unwrap();
        let p = p.read().await;
        assert_eq!(p.state, PipelineState::Complete);
        assert_eq!(p.title, "My Page");
        assert!(p.is_ready());
    }

    #[tokio::test]
    async fn close_removes_children() {
        let c = Constellation::new();
        let parent = c.create_pipeline("https://a.com", None).await;
        let child = c.create_pipeline("https://b.com", Some(parent)).await;
        c.close_pipeline(parent).await;
        assert!(c.get_pipeline(parent).await.is_none());
        assert!(c.get_pipeline(child).await.is_none());
        assert_eq!(c.len().await, 0);
    }

    #[tokio::test]
    async fn list_pipelines_in_order() {
        let c = Constellation::new();
        let a = c.create_pipeline("https://a.com", None).await;
        let b = c.create_pipeline("https://b.com", None).await;
        assert_eq!(c.list_pipelines().await, vec![a, b]);
    }
}
