use std::collections::VecDeque;

use async_trait::async_trait;
use dml::DmlOperation;
use parking_lot::Mutex;

use super::{DmlError, DmlSink};
use crate::data::DmlApplyAction;

#[derive(Debug, Default)]
struct MockDmlSinkState {
    calls: Vec<DmlOperation>,
    ret: VecDeque<Result<DmlApplyAction, DmlError>>,
}

#[derive(Debug, Default)]
pub struct MockDmlSink {
    state: Mutex<MockDmlSinkState>,
}

impl MockDmlSink {
    pub(crate) fn with_apply_return(
        self,
        ret: impl Into<VecDeque<Result<DmlApplyAction, DmlError>>>,
    ) -> Self {
        self.state.lock().ret = ret.into();
        self
    }

    pub fn get_calls(&self) -> Vec<DmlOperation> {
        self.state.lock().calls.clone()
    }
}

#[async_trait]
impl DmlSink for MockDmlSink {
    type Error = DmlError;
    async fn apply(&self, op: DmlOperation) -> Result<DmlApplyAction, DmlError> {
        let mut state = self.state.lock();
        state.calls.push(op);
        state.ret.pop_front().expect("no mock sink value to return")
    }
}
