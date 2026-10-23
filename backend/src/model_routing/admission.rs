// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! A legacy caller must not discard a conversation's routed execution contract.
use uuid::Uuid;
use crate::{AppState, Result};
use super::RoutingError;

pub(crate) async fn chat_requires_routing(state: &AppState, chat: Uuid) -> Result<bool> {
    // Manual intent alone is also written by the compatibility trigger for old
    // llm_provider_id clients. Auto, versioned intent or routed execution opts a
    // conversation out of that legacy contract, including empty new chats.
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM chats WHERE id=$1 AND (model_selection->>'mode'='auto' OR model_selection->>'schema_version'='2')) \
         OR EXISTS(SELECT 1 FROM voice_routed_chats WHERE chat_id=$1) \
         OR EXISTS(SELECT 1 FROM browser_routed_chats WHERE chat_id=$1) \
         OR EXISTS(SELECT 1 FROM api_routed_chats WHERE chat_id=$1) \
         OR EXISTS(SELECT 1 FROM routed_chat_branches WHERE chat_id=$1) \
         OR EXISTS(SELECT 1 FROM messages WHERE chat_id=$1 AND (routing_selection IS NOT NULL OR routing_decision IS NOT NULL)) \
         OR EXISTS(SELECT 1 FROM routed_jobs WHERE context->>'chat_id'=$1::text)"
    ).bind(chat).fetch_one(&state.pg).await?)
}

pub async fn require_legacy_chat(state: &AppState, chat: Option<Uuid>) -> Result<()> {
    if let Some(chat) = chat {
        if chat_requires_routing(state, chat).await? {
            return Err(RoutingError::Incompatible("this conversation requires explicit routed execution").into());
        }
    }
    Ok(())
}
