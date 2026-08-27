use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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

    pub async fn close_pipeline(&self, id: u32) {
        let mut pipelines = self.pipelines.write().await;
        pipelines.remove(&id);
    }
}

impl Default for Constellation {
    fn default() -> Self {
        Self::new()
    }
}
