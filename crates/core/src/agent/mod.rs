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
mod canonical_args;
mod canonical_helpers;
mod defs;
mod event;
mod exec;
mod lifecycle;
mod preflight;
mod stream;
mod todo_reminder;
mod toolphase;
mod turn;
mod usage;

use argbuf::*;
use canonical_args::*;
use canonical_helpers::*;
use defs::*;
use exec::*;
use preflight::*;
use todo_reminder::*;
use usage::*;

pub use defs::{
    context_window_label, is_context_overflow_message, model_context_limit, model_supports_1m,
    Agent, AgentEvent,
};
pub use usage::default_system_prompt;

#[cfg(test)]
mod agent_test_support;
#[cfg(test)]
mod approval_gate_tests;
#[cfg(test)]
mod arg_buffer_tests;
#[cfg(test)]
mod canonical_args_tests;
#[cfg(test)]
mod compaction_tests;
#[cfg(test)]
mod context_limit_tests;
#[cfg(test)]
mod permission_gate_tests;
#[cfg(test)]
mod tool_call_wrapper_tests;
#[cfg(test)]
mod tool_exec_tests;
#[cfg(test)]
mod tool_result_tests;
