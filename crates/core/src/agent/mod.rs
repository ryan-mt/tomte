use std::sync::Arc;
use std::time::Duration;

use crate::client::LlmClient;
use crate::config::Config;
use crate::openai::{InputItem, MessageContent, ResponseStreamEvent, ResponsesRequest};
use crate::tool_args::{accumulate_argument_fragment, normalize_argument_fragment};
use crate::tools::{ApprovalMode, Registry, SessionState, TodoItem, TodoStatus, ToolContext};
use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

mod argbuf;
mod canonical;
mod canonical2;
mod defs;
mod event;
mod exec;
mod lifecycle;
mod stream;
mod toolphase;
mod turn;
mod usage;

use argbuf::*;
use canonical::*;
use canonical2::*;
use defs::*;
use exec::*;
use usage::*;

pub use defs::{
    context_window_label, is_context_overflow_message, model_context_limit, model_supports_1m,
    Agent, AgentEvent,
};
pub use usage::default_system_prompt;

#[cfg(test)]
mod approval_gate_tests;
#[cfg(test)]
mod compaction_tests;
#[cfg(test)]
mod context_limit_tests;
#[cfg(test)]
mod fcid_a;
#[cfg(test)]
mod fcid_b;
#[cfg(test)]
mod fcid_c;
#[cfg(test)]
mod fcid_common;
#[cfg(test)]
mod fcid_d;
#[cfg(test)]
mod permission_gate_tests;
#[cfg(test)]
mod tool_result_tests;
