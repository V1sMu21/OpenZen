use oz_core_types::{ContentBlock, LlmClient, Message, Role};
use serde_json::Value;

/// Configuration for context compression. All budgets are in TOKENS.
#[derive(Debug, Clone)]
pub struct CompressionConfig {
    /// Minimum number of messages to keep (never compress below this)
    pub min_messages: usize,
    /// Percentage of context window that triggers compression (0-100).
    /// Default 80: at 80% of a 256K window (~205K), leaves 20% (~51K)
    /// headroom for the model's response and tool outputs.
    pub trigger_pct: u8,
    /// Hard ceiling in tokens — compression always triggers at or above
    /// this count, regardless of context window size. Prevents local
    /// inference engines from choking on huge prefill.
    pub max_trigger_tokens: usize,
    /// Emergency ceiling — if token count exceeds this, force aggressive
    /// compression immediately (no LLM summary, just template). This is
    /// checked both at turn start AND after tool execution to prevent
    /// context explosion during a single turn.
    pub hard_max_tokens: usize,
    /// Max tokens to keep per tool result before truncation
    pub tool_result_budget: usize,
    /// Max tokens to keep per old assistant response
    pub old_assistant_budget: usize,
    /// Max tokens to keep per old user message
    pub old_user_budget: usize,
    /// Whether to enable summarization of old tool results
    pub enable_summarization: bool,
    /// Compression target — after compression, total tokens are reduced
    /// to AT OR BELOW this level. Deliberately far below the trigger
    /// line: a compression that stops at the trigger line (old behavior)
    /// saves a few K tokens and re-triggers next turn (170K → 166K → 170K
    /// thrash). Mirrors OpenCode's tail-preserve budget (usable×0.25,
    /// capped ~8K); 16K ≈ 6% of a 256K window is generous for local
    /// tool outputs. One deep compression buys a long stable period,
    /// and the omlx prefix-cache chain is broken only once instead of
    /// every turn.
    pub target_tokens: usize,
    /// How long the agent loop waits (bounded) for the LLM summary
    /// after messages were actually dropped. When the summary arrives
    /// in time, the main model's prefill runs against the REAL summary
    /// instead of the template. On timeout the template is used and
    /// collection continues in the background for the next turn.
    /// 10 min: merge cost scales superlinearly with the removed-window
    /// size, and a 1M-token window on a small local summarizer
    /// (e.g. LFM2.5-230M) needs far more than the old 60s.
    pub summary_wait_secs: u64,
    /// Cap for the summary prompt in chars. The agent loop no longer
    /// truncates the prompt (full removed window is fed to
    /// `spawn_summary`, which splits oversized prompts via
    /// `progressive_merge_summary`); kept for tooling callers.
    pub summary_max_prompt_chars: usize,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        CompressionConfig {
            min_messages: 8,
            trigger_pct: 80,
            tool_result_budget: 3000, // ~12K chars — enough for file contents
            old_assistant_budget: 2000, // ~8K chars — preserve planning context
            old_user_budget: 5000,    // ~20K chars — never truncate task descriptions
            enable_summarization: true,
            // Absolute ceiling on the trigger line. 0 (the default) = no
            // absolute cap: the threshold is context_win × trigger_pct
            // (80% of the model's window), so a 1M-context model compresses
            // at 800K instead of being clamped to the old 170K constant
            // (which made 80% of a big window unreachable). Set a non-zero
            // value only to protect a model whose real capacity is smaller
            // than its configured window.
            max_trigger_tokens: 0,
            hard_max_tokens: 0,
            // ~6% of a 256K window. Deep compression: 170K trigger → ~16K
            // after, one compression buys a long stable period.
            target_tokens: 16_000,
            summary_wait_secs: 600,
            // Kept for compatibility; the agent loop no longer truncates the
            // summary prompt (full removed window is fed to spawn_summary,
            // which splits oversized prompts via progressive_merge_summary).
            summary_max_prompt_chars: 12_000,
        }
    }
}

/// Statistics about message content for compression decisions.
/// Char counts are tracked internally; token estimates are derived from
/// the LLM-reported ratio (known_tokens / total_chars), which auto-calibrates
/// to any model's tokenizer with zero heuristic error.
#[derive(Debug, Default)]
pub struct UsageStats {
    pub total_chars: usize,
    pub tool_result_chars: usize,
    pub assistant_chars: usize,
    pub user_chars: usize,
    pub system_chars: usize,
    pub message_count: usize,
    /// chars-to-tokens ratio from the LLM's usage report.
    /// When set, `chars × ratio ≈ tokens` for this conversation.
    pub token_ratio: f64,
}

impl UsageStats {
    /// Estimated total tokens using the auto-calibrated ratio.
    /// Falls back to chars/4 (classic English heuristic) only when ratio is unknown.
    pub fn total_tokens(&self) -> usize {
        self.chars_to_tokens(self.total_chars)
    }

    /// Convert chars to estimated tokens using the calibrated ratio.
    pub fn chars_to_tokens(&self, chars: usize) -> usize {
        if self.token_ratio > 0.0 {
            (chars as f64 * self.token_ratio) as usize
        } else {
            chars / 4 // fallback when no LLM ratio available
        }
    }

    /// Convert a token budget to a char budget for trimming.
    pub fn tokens_to_chars(&self, tokens: usize) -> usize {
        if self.token_ratio > 0.0 {
            (tokens as f64 / self.token_ratio) as usize
        } else {
            tokens * 4 // fallback
        }
    }
}

/// Measure the character usage of a message list.
/// If `stored_ratio` is Some(r), it's used as the token_ratio for this measurement.
pub fn measure_usage(messages: &[Message]) -> UsageStats {
    measure_usage_with_ratio(messages, None)
}

pub fn measure_usage_with_ratio(messages: &[Message], stored_ratio: Option<f64>) -> UsageStats {
    let mut stats = UsageStats {
        message_count: messages.len(),
        token_ratio: stored_ratio.unwrap_or(0.0),
        ..UsageStats::default()
    };

    for msg in messages {
        for block in &msg.content {
            let len = content_block_len(block);
            stats.total_chars += len;
            match msg.role {
                Role::System => stats.system_chars += len,
                Role::User => stats.user_chars += len,
                Role::Assistant => stats.assistant_chars += len,
                Role::Tool => stats.tool_result_chars += len,
            }
        }
    }
    stats
}

fn content_block_len(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text, .. } => text.len(),
        ContentBlock::ToolUse { name, input, .. } => {
            name.len() + serde_json::to_string(input).unwrap_or_default().len()
        }
        ContentBlock::ToolResult { content, .. } => match content {
            oz_core_types::ContentContainer::Text(t) => t.len(),
            oz_core_types::ContentContainer::Blocks(bs) => bs.iter().map(content_block_len).sum(),
        },
        _ => 0,
    }
}

/// Scan messages for ToolResult blocks whose tool_use_id doesn't
/// appear in the preceding assistant message. Convert orphans to
/// plain text so the LLM API doesn't reject them with
/// "missing field tool_call_id".
fn repair_orphaned_tool_results(messages: &mut [Message]) {
    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        if msg.role != Role::User {
            i += 1;
            continue;
        }
        // Collect tool_use_ids from the preceding assistant message
        let valid_ids: std::collections::HashSet<String> =
            if i > 0 && messages[i - 1].role == Role::Assistant {
                messages[i - 1]
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect()
            } else {
                std::collections::HashSet::new()
            };
        // Repair: convert orphaned ToolResult blocks to text
        let mut repaired = false;
        for block in messages[i].content.iter_mut() {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } = block
            {
                if !valid_ids.contains(tool_use_id.as_str()) {
                    let text = match content {
                        oz_core_types::ContentContainer::Text(t) => std::mem::take(t),
                        oz_core_types::ContentContainer::Blocks(bs) => bs
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text { text, .. } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    *block = ContentBlock::text(format!("[compressed tool output]: {text}"));
                    repaired = true;
                }
            }
        }
        if repaired {
            // Compact consecutive Text blocks into one
            let mut compacted: Vec<ContentBlock> = Vec::new();
            for block in messages[i].content.drain(..) {
                if let ContentBlock::Text { text, .. } = block {
                    if let Some(ContentBlock::Text { text: prev, .. }) = compacted.last_mut() {
                        prev.push('\n');
                        prev.push_str(&text);
                    } else {
                        compacted.push(ContentBlock::text(&text));
                    }
                } else {
                    compacted.push(block);
                }
            }
            messages[i].content = compacted;
        }
        i += 1;
    }
}

/// Compress messages when token usage exceeds config.trigger_pct% of context window.
///
/// Strategy (OpenCode-style, information-density-first):
/// 1. If token usage ≤ trigger threshold, do nothing.
/// 2. Phase 1: Trim verbose tool results to keep only key output.
/// 3. Phase 2: Trim old assistant/user messages, keeping recent pairs intact.
/// 4. Phase 3: Drop oldest turns down to `target_tokens` (far below the
///    trigger line), preserving the last `tail_turns` user turns. The
///    removed window is exactly the messages the caller summarizes, so
///    one compression yields a long stable period instead of shaving a
///    few K and re-triggering next turn.
///
/// `context_win` is in TOKENS. When `known_tokens` is provided (from the
/// LLM provider's last usage report), the chars→tokens ratio is auto-calibrated
/// as `known_tokens / total_chars`, giving model-accurate token estimates without
/// any hardcoded heuristic.
pub fn compress_messages(
    messages: &mut Vec<Message>,
    context_win: usize,
    config: &CompressionConfig,
    known_tokens: Option<usize>,
) -> usize {
    let pct_trigger = context_win * config.trigger_pct as usize / 100;
    // 0 = no absolute cap — the threshold is pure win × trigger_pct.
    let trigger_tokens = if config.max_trigger_tokens > 0 {
        pct_trigger.min(config.max_trigger_tokens)
    } else {
        pct_trigger
    };
    // Target must sit strictly below the trigger line (emergency mode
    // passes context_win=1 → trigger=0 → target=1, deleting down to the
    // min_messages floor, same as before).
    let target_tokens = config
        .target_tokens
        .min(trigger_tokens.saturating_sub(1).max(1));

    let raw_stats = measure_usage(messages);
    // Auto-calibrate chars→tokens ratio from LLM's exact token count.
    let token_ratio = known_tokens
        .filter(|&kt| kt > 0 && raw_stats.total_chars > 0)
        .map(|kt| kt as f64 / raw_stats.total_chars as f64);
    let stats = measure_usage_with_ratio(messages, token_ratio);
    let est_tokens = stats.total_tokens();

    if est_tokens <= trigger_tokens {
        return 0;
    }

    let mut saved: usize = 0;

    // Phase 1: Trim verbose tool results.
    let tool_result_char_budget = stats.tokens_to_chars(config.tool_result_budget);
    if config.enable_summarization && stats.tool_result_chars > tool_result_char_budget {
        saved += compress_tool_results(messages, tool_result_char_budget);
    }

    // Phase 2: Trim old messages. Keep the most recent pairs untouched
    // (recency is the best indicator of relevance).
    let keep_recent = 2;
    let sys_offset = messages
        .iter()
        .take_while(|m| m.role == Role::System)
        .count();
    let trim_end = messages.len().saturating_sub(keep_recent);
    for msg in &mut messages[sys_offset..trim_end] {
        saved += trim_message_content(msg, config, &stats);
    }

    // Phase 3: If still over target, drop the oldest turns. Keep the
    // system prompt and the last `tail_turns` user turns — recent
    // context is the most relevant, and the removed middle section is
    // exactly what the caller summarizes. Stop at `target_tokens` (far
    // below the trigger line) so one compression buys a long stable
    // period instead of shaving 4K and re-triggering next turn.
    let tail_turns = 2;
    let user_positions: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();
    // Index of the first message that must survive (start of tail). It
    // shrinks as messages are removed so it keeps pointing at the same
    // surviving messages.
    let mut keep_start = if user_positions.len() > tail_turns {
        user_positions[user_positions.len() - tail_turns]
    } else {
        messages.len() // fewer turns than the tail → nothing to drop
    };
    let drop_start = sys_offset;
    let mut removed: usize = 0;
    while drop_start < keep_start
        && messages.len() > drop_start + config.min_messages
        && measure_usage_with_ratio(messages, token_ratio).total_tokens() > target_tokens
    {
        messages.remove(drop_start);
        removed += 1;
        keep_start -= 1;
        while drop_start < keep_start
            && messages[drop_start].role != Role::User
            && messages[drop_start].role != Role::System
        {
            messages.remove(drop_start);
            removed += 1;
            keep_start -= 1;
        }
        if removed > 0 && drop_start < messages.len() {
            sanitize_leading_message(&mut messages[drop_start]);
        }
    }

    // Phase 4: Clean up orphaned tool_result blocks. When Phase 3
    // removes an assistant(tool_use) message but keeps the paired
    // user(tool_result), the API rejects the request with
    // "missing field tool_call_id". Scan for ToolResult blocks whose
    // tool_use_id doesn't appear in the preceding assistant message
    // and convert them to text.
    repair_orphaned_tool_results(messages);

    saved + removed
}

/// Trim a single message's content to keep only the most relevant portion.
/// Uses auto-calibrated ratio for accurate token→chars budget conversion.
fn trim_message_content(
    msg: &mut Message,
    config: &CompressionConfig,
    stats: &UsageStats,
) -> usize {
    let token_budget = match msg.role {
        Role::Assistant => config.old_assistant_budget,
        Role::User => config.old_user_budget,
        _ => return 0,
    };
    let char_budget = stats.tokens_to_chars(token_budget);

    let original_len: usize = msg.content.iter().map(content_block_len).sum();
    if original_len <= char_budget {
        return 0;
    }

    let mut new_blocks = Vec::new();
    let mut used = 0;
    for block in msg.content.drain(..) {
        let block_len = content_block_len(&block);
        if used + block_len <= char_budget {
            used += block_len;
            new_blocks.push(block);
        } else if let ContentBlock::Text {
            text,
            cache_control,
        } = block
        {
            let remaining = char_budget.saturating_sub(used);
            if remaining > 0 {
                let mut t = text;
                // `remaining` is a char budget, but `String::truncate`
                // requires a byte boundary. Clamp to the largest char
                // boundary ≤ remaining — otherwise multi-byte UTF-8
                // (e.g. Chinese, 3 bytes/char) panics with
                // "assertion failed: self.is_char_boundary(new_len)".
                t.truncate(t.floor_char_boundary(remaining));
                new_blocks.push(ContentBlock::Text {
                    text: t,
                    cache_control,
                });
            }
            break;
        }
    }
    msg.content = new_blocks;
    original_len.saturating_sub(char_budget)
}

/// Compress tool result content by truncating verbose outputs.
fn compress_tool_results(messages: &mut [Message], budget: usize) -> usize {
    let mut saved = 0;
    let mut accumulated = 0usize;

    for msg in messages.iter_mut() {
        if msg.role != Role::Tool && msg.role != Role::Assistant {
            continue;
        }
        if accumulated > budget {
            break;
        }

        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                let original_len = match content {
                    oz_core_types::ContentContainer::Text(t) => t.len(),
                    oz_core_types::ContentContainer::Blocks(bs) => {
                        bs.iter().map(content_block_len).sum()
                    }
                };

                let truncated = truncate_content(content, budget.saturating_sub(accumulated));
                let new_len = match &truncated {
                    oz_core_types::ContentContainer::Text(t) => t.len(),
                    oz_core_types::ContentContainer::Blocks(bs) => {
                        bs.iter().map(content_block_len).sum()
                    }
                };

                accumulated += original_len;
                saved += original_len.saturating_sub(new_len);
                *content = truncated;
            }

            if let ContentBlock::Text { text, .. } = block {
                if text.len() > budget / 4 {
                    let half = text.floor_char_boundary(text.len() / 4);
                    let new_text = format!(
                        "{}...[+{} more chars]",
                        &text[..half],
                        text.len() - half * 2
                    );
                    saved += text.len().saturating_sub(new_text.len());
                    *block = ContentBlock::text(&new_text);
                }
            }
        }
    }

    saved
}

fn truncate_content(
    content: &oz_core_types::ContentContainer,
    budget: usize,
) -> oz_core_types::ContentContainer {
    match content {
        oz_core_types::ContentContainer::Text(t) => {
            if t.len() > budget {
                let half = t.floor_char_boundary(budget / 2);
                let tail_start = t.floor_char_boundary(t.len().saturating_sub(half));
                oz_core_types::ContentContainer::Text(format!(
                    "{}...[truncated {} chars]...{}",
                    &t[..half],
                    t.len() - budget,
                    &t[tail_start..]
                ))
            } else {
                content.clone()
            }
        }
        oz_core_types::ContentContainer::Blocks(bs) => {
            let mut new_blocks: Vec<ContentBlock> = Vec::new();
            let mut used = 0usize;
            for block in bs {
                if used >= budget {
                    new_blocks.push(ContentBlock::text(format!(
                        "[... {} more blocks truncated]",
                        bs.len() - new_blocks.len()
                    )));
                    break;
                }
                let block_len = content_block_len(block);
                if let ContentBlock::Text { text, .. } = block {
                    if used + block_len > budget {
                        let remaining = budget.saturating_sub(used);
                        let truncated = if text.len() > remaining {
                            let half = text.floor_char_boundary(remaining / 2);
                            format!("{}...[truncated]", &text[..half])
                        } else {
                            text.clone()
                        };
                        new_blocks.push(ContentBlock::text(&truncated));
                        used += remaining;
                    } else {
                        new_blocks.push(block.clone());
                        used += block_len;
                    }
                } else {
                    if used + block_len <= budget {
                        new_blocks.push(block.clone());
                        used += block_len;
                    }
                }
            }
            oz_core_types::ContentContainer::Blocks(new_blocks)
        }
    }
}

fn sanitize_leading_message(msg: &mut Message) {
    let texts: Vec<String> = msg
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.clone()),
            ContentBlock::ToolResult { content, .. } => match content {
                oz_core_types::ContentContainer::Text(t) => Some(t.clone()),
                oz_core_types::ContentContainer::Blocks(bs) => Some(
                    bs.iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            },
            _ => None,
        })
        .collect();
    msg.content = vec![ContentBlock::text(texts.join("\n"))];
}

/// Emergency compression — applies aggressive trimming immediately when
/// token count exceeds `hard_max_tokens`. Skips LLM summary entirely
/// (template only) to avoid blocking the agent loop during a critical
/// context overflow. Returns (saved_chars, template_summary).
pub fn emergency_compress(
    messages: &mut Vec<Message>,
    _context_win: usize,
    config: &CompressionConfig,
    known_tokens: Option<usize>,
) -> (usize, String) {
    let raw_stats = measure_usage(messages);
    let token_ratio = known_tokens
        .filter(|&kt| kt > 0 && raw_stats.total_chars > 0)
        .map(|kt| kt as f64 / raw_stats.total_chars as f64);
    let _stats = measure_usage_with_ratio(messages, token_ratio);

    let snapshot_before: Vec<serde_json::Value> = messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    let before_count = messages.len();

    // Use aggressive budgets for emergency mode
    let mut emergency_config = config.clone();
    emergency_config.tool_result_budget = config.tool_result_budget / 2;
    emergency_config.old_assistant_budget = config.old_assistant_budget / 2;
    emergency_config.old_user_budget = config.old_user_budget / 2;

    // Force trigger by using a tiny context_win
    let saved = compress_messages(messages, 1, &emergency_config, known_tokens);

    let removed_count = before_count.saturating_sub(messages.len());
    let summary_json = if removed_count > 0 {
        &snapshot_before[..removed_count.min(snapshot_before.len())]
    } else {
        &snapshot_before[..]
    };

    let mut template = build_compression_summary(summary_json, "");
    let previous = extract_compression_summaries(messages);
    if !previous.is_empty() {
        template =
            format!("[Prior context (merge into summary below)]:\n{previous}\n\n---\n\n{template}");
    }

    if !template.is_empty() {
        let inject_at = messages
            .iter()
            .position(|m| m.role == Role::User || m.role == Role::Assistant)
            .unwrap_or(0);
        messages.insert(
            inject_at,
            Message::system(format!("[Compression summary (emergency)]: {template}")),
        );
    }

    (saved, template)
}

/// Compression service for offloading LLM summary generation to a
/// background task. The agent loop calls `spawn_summary` to fire an
/// async summary request without blocking, then `collect_summary` on a
/// subsequent turn to inject the result.
pub struct CompressionService {
    pub summary_model_name: Option<String>,
    pub summary_apibase: Option<String>,
    pub summary_apikey: Option<String>,
    pub lang: String,
}

impl CompressionService {
    pub fn new(
        model: Option<String>,
        apibase: Option<String>,
        apikey: Option<String>,
        lang: String,
    ) -> Self {
        CompressionService {
            summary_model_name: model,
            summary_apibase: apibase,
            summary_apikey: apikey,
            lang,
        }
    }

    pub fn is_configured(&self) -> bool {
        self.summary_model_name.is_some()
    }

    /// Spawn a background task to generate an LLM summary. Returns a
    /// oneshot receiver that the agent loop can poll on subsequent turns
    /// without blocking.
    ///
    /// When `full_prompt` exceeds ~12K chars the summary model's prefill
    /// becomes too slow for the 30s timeout. Instead of failing, we split
    /// the prompt into paragraph-aligned chunks and merge them progressively
    /// via multiple small LLM calls, each well within the timeout budget.
    pub fn spawn_summary(
        &self,
        full_prompt: String,
        template: String,
    ) -> tokio::sync::oneshot::Receiver<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let model_name = match &self.summary_model_name {
            Some(n) => n.clone(),
            None => {
                let _ = tx.send(template);
                return rx;
            }
        };
        let apibase = self.summary_apibase.clone().unwrap_or_default();
        let apikey = self.summary_apikey.clone().unwrap_or_default();
        let lang = self.lang.clone();

        tokio::spawn(async move {
            // Bound concurrent summary work: local summary models share the
            // GPU with the main agent, so unbounded parallel summaries from
            // multiple sessions starve the real run.
            static SUMMARY_SEM: std::sync::OnceLock<tokio::sync::Semaphore> =
                std::sync::OnceLock::new();
            let sem = SUMMARY_SEM.get_or_init(|| tokio::sync::Semaphore::new(2));
            let Ok(_permit) = sem.acquire().await else {
                return;
            };
            let summary_fut = async {
                if full_prompt.len() > 12_000 {
                    progressive_merge_summary(
                        &full_prompt,
                        &template,
                        &model_name,
                        &apibase,
                        &apikey,
                        &lang,
                    )
                    .await
                } else {
                    call_summary_llm(
                        &full_prompt,
                        &model_name,
                        &apibase,
                        &apikey,
                        &template,
                        &lang,
                    )
                    .await
                }
            };
            tokio::pin!(summary_fut);
            // The Sender is shared through a mutex so the closed-watcher
            // below can poll is_closed() without conflicting with the
            // by-value send in the other select branch.
            let tx_shared = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
            let tx_watch = std::sync::Arc::clone(&tx_shared);
            tokio::select! {
                summary = &mut summary_fut => {
                    if let Some(tx) = lock_sender_tx(&tx_shared).take() {
                        let _ = tx.send(summary);
                    }
                }
                // The run finished without collecting the result: cancel
                // the summary instead of burning LLM capacity on a result
                // nobody will read.
                _ = async move {
                    loop {
                        let closed = lock_sender_tx(&tx_watch)
                            .as_ref()
                            .map(|t| t.is_closed())
                            .unwrap_or(true);
                        if closed {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                } => {}
            }
        });

        rx
    }

    /// Non-blocking check: if the receiver has a ready summary, return
    /// it. Otherwise return None.
    pub fn try_collect(rx: &mut tokio::sync::oneshot::Receiver<String>) -> Option<String> {
        match rx.try_recv() {
            Ok(s) => Some(s),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => None,
        }
    }

    /// Blocking collection — only for cases where we MUST have a summary.
    pub async fn collect(rx: tokio::sync::oneshot::Receiver<String>) -> String {
        rx.await.unwrap_or_default()
    }
}

/// ── Progressive merge helpers ──
/// Used by `CompressionService::spawn_summary` to handle prompts too
/// large for a single summary-model call.
///
/// Split text into paragraph-aligned chunks ≤ `max_chunk` chars each.
fn split_into_chunks(text: &str, max_chunk: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for paragraph in text.split("\n\n") {
        if current.len() + paragraph.len() > max_chunk && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(paragraph);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn lock_sender_tx(
    m: &std::sync::Mutex<Option<tokio::sync::oneshot::Sender<String>>>,
) -> std::sync::MutexGuard<'_, Option<tokio::sync::oneshot::Sender<String>>> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Single LLM call: summarize `content` into a concise markdown summary.
/// Returns the LLM output on success, `fallback` on failure or empty response.
async fn call_summary_llm(
    content: &str,
    model: &str,
    apibase: &str,
    apikey: &str,
    fallback: &str,
    lang: &str,
) -> String {
    let secs = ((content.len() / 100).min(30) as u64).max(5);
    let config = oz_config::mykey::SessionConfig {
        model: model.to_string(),
        apibase: apibase.to_string(),
        apikey: apikey.to_string(),
        context_win: 65536,
        max_tokens: Some(2048),
        temperature: Some(0.3),
        api_mode: Default::default(),
        reasoning_effort: None,
        max_retries: Some(0),
        proxy: None,
        verify: None,
        timeout: Some(secs),
        llm_nos: None,
        base_delay: None,
        spring_back: None,
    };
    let backend: Box<dyn oz_llm::Session> = Box::new(oz_llm::NativeOAISession::new(config));
    let instruction = if lang == "zh" {
        "用简体中文将下面的对话总结为一份简洁的 markdown 记录。\
         保留所有关键信息。\n\n\
         ## 必需段落\n\
         ### 任务 — 原始请求、目标、约束\n\
         ### 文件与路径 — 每个创建/修改/读取的文件及完整路径\n\
         ### 关键决策 — 架构、库、方案\n\
         ### 进度 — 已完成 vs 未完成\n\
         ### 最近动作 — 最后 2-3 个工具调用与结果\n\
         ### 用户消息 — 澄清、反馈、新指令"
    } else {
        "Summarize the conversation below into a concise markdown record. \
         Preserve ALL essential information.\n\n\
         ## REQUIRED SECTIONS\n\
         ### Task — original request, goals, constraints\n\
         ### Files & Paths — every file created/modified/read with full path\n\
         ### Key Decisions — architecture, libraries, approaches\n\
         ### Progress — completed vs remaining\n\
         ### Recent Actions — last 2-3 tool calls and results\n\
         ### User Messages — clarifications, feedback, new instructions"
    };
    let prompt = Message::user(format!("{instruction}\n\n---\n\n{content}"));
    let mut sc = oz_llm::NativeToolClient::new(backend);
    match tokio::time::timeout(
        std::time::Duration::from_secs(secs),
        sc.chat(&[prompt], &[]),
    )
    .await
    {
        Ok(Ok(resp)) if !resp.content.is_empty() => resp.content,
        _ => fallback.to_string(),
    }
}

/// Merge a large prompt into a single summary by splitting into small
/// chunks and merging them pair-wise via multiple LLM calls.
async fn progressive_merge_summary(
    full_prompt: &str,
    template: &str,
    model: &str,
    apibase: &str,
    apikey: &str,
    lang: &str,
) -> String {
    const CHUNK_SIZE: usize = 7000;
    let chunks = split_into_chunks(full_prompt, CHUNK_SIZE);
    if chunks.len() <= 1 {
        return call_summary_llm(full_prompt, model, apibase, apikey, template, lang).await;
    }

    // Round 1: merge adjacent chunks in pairs
    let mut merged: Vec<String> = Vec::new();
    for pair in chunks.chunks(2) {
        let input = if pair.len() == 1 {
            pair[0].clone()
        } else {
            format!("{}\n\n{}", pair[0], pair[1])
        };
        merged.push(call_summary_llm(&input, model, apibase, apikey, &input, lang).await);
    }

    // Rounds 2+: keep merging until one result remains
    while merged.len() > 1 {
        let mut next: Vec<String> = Vec::new();
        for pair in merged.chunks(2) {
            let input = if pair.len() == 1 {
                pair[0].clone()
            } else {
                format!("{}\n\n{}", pair[0], pair[1])
            };
            next.push(call_summary_llm(&input, model, apibase, apikey, &input, lang).await);
        }
        merged = next;
    }

    merged
        .into_iter()
        .next()
        .unwrap_or_else(|| template.to_string())
}

/// Schema B: Match surviving compressed messages back to original JSON entries.
///
/// `compress_messages` removes from the front (after system prompts), so
/// survivors are at the tail. We iterate both lists from the back, matching
/// by role + content prefix (Phase 1 truncation shortens text, never extends).
pub fn match_messages_to_originals(compressed: &[Message], originals: &[Value]) -> Vec<Value> {
    if compressed.len() == originals.len() {
        return originals.to_vec();
    }

    let mut surviving: Vec<Value> = Vec::with_capacity(compressed.len());
    let mut orig_idx = originals.len();

    for msg in compressed.iter().rev() {
        let role_str = msg.role.as_str();
        let msg_text = msg.content_text();

        while orig_idx > 0 {
            orig_idx -= 1;
            let orig = &originals[orig_idx];
            let orig_role = orig.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if orig_role != role_str {
                continue;
            }
            let orig_content = orig.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if content_prefix_matches(orig_content, &msg_text) {
                surviving.push(orig.clone());
                break;
            }
        }
    }

    surviving.reverse();
    surviving
}

fn content_prefix_matches(original: &str, compressed: &str) -> bool {
    if original.len() <= compressed.len() {
        compressed.starts_with(original)
    } else {
        original.starts_with(compressed)
    }
}

// ── Compression Summary Generation ──

/// Extract all `[Compression summary]` system messages from the message list,
/// remove them in place, and return their concatenated bodies. The caller
/// (`spawn_summary`) handles progressive LLM merging when the result is too
/// large for a single summary-model call.
pub fn extract_compression_summaries(messages: &mut Vec<Message>) -> String {
    let prefix = "[Compression summary]";
    let mut bodies: Vec<String> = Vec::new();
    messages.retain(|m| {
        let text = m.content_text().trim().to_string();
        if text.starts_with(prefix) {
            let body = text
                .strip_prefix(&format!("{prefix}:"))
                .or_else(|| text.strip_prefix(prefix))
                .unwrap_or(&text)
                .trim()
                .to_string();
            if !body.is_empty() {
                bodies.push(body);
            }
            false
        } else {
            true
        }
    });
    if bodies.is_empty() {
        String::new()
    } else if bodies.len() == 1 {
        bodies.into_iter().next().unwrap()
    } else {
        bodies.join("\n\n---\n[Previous compression]\n---\n\n")
    }
}
/// Unlike the template summary (role counts + preview), this extracts the full
/// text of every removed message so the LLM can generate a meaningful summary.
///
/// Format: `[role]: text content <tool_use:name> input\n[tool_result]: output`
pub fn build_compression_prompt(removed_messages: &[Value]) -> String {
    if removed_messages.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::with_capacity(removed_messages.len() * 2);

    for msg in removed_messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = msg.get("content");
        let blocks: &[Value] = match content {
            Some(c) if c.is_array() => c.as_array().unwrap(),
            _ => continue,
        };

        for block in blocks {
            if let Some(text) = block
                .get("text")
                .and_then(|t| t.get("text"))
                .and_then(|t| t.as_str())
            {
                let t = text.trim();
                if !t.is_empty() && !t.starts_with("[Compression summary]") {
                    lines.push(format!("[{role}]: {t}"));
                }
            }

            if let Some(tool_use) = block.get("tool_use") {
                let name = tool_use.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                let input = tool_use
                    .get("input")
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .unwrap_or_default();
                // Truncate tool input to keep prompt manageable.
                // MUST use char boundary — slicing at byte offset 200
                // can cut through a multi-byte CJK character (e.g. 目=3 bytes)
                // causing a panic that poisons the mutex and leaves the
                // session permanently "Running".
                let input_short = if input.len() > 200 {
                    let end = input
                        .char_indices()
                        .nth(200)
                        .map(|(i, _)| i)
                        .unwrap_or(input.len());
                    format!("{}...", &input[..end])
                } else {
                    input
                };
                lines.push(format!("[{role}]: <tool_use:{name}> {input_short}"));
            }

            if let Some(tool_result) = block.get("tool_result") {
                let result_text = tool_result
                    .get("content")
                    .and_then(|c| c.as_str())
                    .or_else(|| {
                        tool_result
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(|t| t.as_str())
                    })
                    .unwrap_or("");
                let result_short = if result_text.len() > 300 {
                    let end = result_text
                        .char_indices()
                        .nth(300)
                        .map(|(i, _)| i)
                        .unwrap_or(result_text.len());
                    format!("{}...", &result_text[..end])
                } else {
                    result_text.to_string()
                };
                lines.push(format!("[tool_result]: {result_short}"));
            }
        }
    }

    lines.join("\n")
}

/// Build a structured fallback summary from removed messages.
/// Extracts task, files touched, tool actions, progress, and user messages
/// — providing meaningful context even when the LLM summary fails.
/// `working_dir` anchors the task spec (spec-first): when a `task_spec.md`
/// exists there, the summary points the model back to it so long tasks do
/// not drift from the original spec. Empty string skips the reference.
pub fn build_compression_summary(removed_messages: &[Value], working_dir: &str) -> String {
    if removed_messages.is_empty() {
        return String::new();
    }

    let mut task = String::new();
    let mut files: Vec<String> = Vec::new();
    let mut actions: Vec<String> = Vec::new();
    let mut recent_errors: Vec<String> = Vec::new();
    let mut user_msgs: Vec<String> = Vec::new();
    let mut progress_items: Vec<String> = Vec::new();
    let mut seq = 0usize;

    for msg in removed_messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = msg.get("content");
        let blocks: &[Value] = match content {
            Some(c) if c.is_array() => c.as_array().unwrap(),
            _ => continue,
        };

        // Capture first user message as the task. 800 chars (was 200) so
        // the original spec survives repeated compressions long enough for
        // the task_spec.md anchor (below) to take over.
        if task.is_empty() && role == "user" {
            for block in blocks {
                if let Some(text) = extract_text(block) {
                    if !text.starts_with("[Compression summary]") {
                        task = truncate_safe(text, 800);
                        break;
                    }
                }
            }
        }

        // Collect user messages
        if role == "user" {
            for block in blocks {
                if let Some(text) = extract_text(block) {
                    let t = text.trim().to_string();
                    if !t.is_empty()
                        && !t.starts_with("[Compression summary]")
                        && !t.starts_with("<tool_")
                    {
                        user_msgs.push(truncate_safe(&t, 120));
                    }
                }
            }
        }

        for block in blocks {
            // Extract file paths from tool_use
            if let Some(tool_use) = block.get("tool_use") {
                seq += 1;
                let name = tool_use.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                let input = tool_use.get("input");

                // Extract file_path(s) from tool input
                let paths = extract_paths(input);
                for p in &paths {
                    let entry = format!("  {}: {}", name, p);
                    if !files.contains(&entry) {
                        files.push(entry);
                    }
                }

                // Build action summary
                let action = if paths.is_empty() {
                    let brief = input_desc(input);
                    format!("{}. {} {}", seq, name, brief)
                } else {
                    format!("{}. {} → {}", seq, name, paths.join(", "))
                };
                actions.push(truncate_safe(&action, 150));

                // Track progress from todowrite/todoupdate
                if name == "todowrite" || name == "todoupdate" {
                    if let Some(input) = input {
                        extract_todo_items(input, &mut progress_items);
                    }
                }
            }

            // Extract tool result previews
            if let Some(tool_result) = block.get("tool_result") {
                let result_text = tool_result
                    .get("content")
                    .and_then(|c| c.as_str())
                    .or_else(|| {
                        tool_result
                            .get("content")
                            .and_then(|c| c.get("text"))
                            .and_then(|t| t.as_str())
                    })
                    .unwrap_or("");
                // Reflexion: keep recent tool errors in the summary so a
                // compressed long task does not forget what failed.
                let lower = result_text.to_lowercase();
                if !result_text.is_empty()
                    && (lower.contains("error")
                        || lower.contains("failed")
                        || lower.contains("exception")
                        || lower.contains("traceback"))
                {
                    recent_errors.push(truncate_safe(result_text, 220));
                }
                if !result_text.is_empty() && result_text != "written" && result_text != "ok" {
                    // Only add non-trivial results to actions
                    if let Some(last) = actions.last_mut() {
                        let summary = truncate_safe(result_text, 80);
                        if !summary.is_empty() && summary != "written" && summary != "ok" {
                            *last = format!("{} → \"{}\"", last, summary);
                        }
                    }
                }
            }
        }
    }

    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        "[Compressed: {} messages removed]",
        removed_messages.len()
    ));

    if !task.is_empty() {
        parts.push(format!("## Task\n{}", task));
    }

    if !files.is_empty() {
        // Dedup and limit
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<&String> = files.iter().filter(|f| seen.insert(*f)).collect();
        let limited = if unique.len() > 30 {
            &unique[..30]
        } else {
            &unique
        };
        parts.push(format!(
            "## Files\n{}",
            limited
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !actions.is_empty() {
        let limited = if actions.len() > 20 {
            &actions[..20]
        } else {
            &actions
        };
        parts.push(format!("## Actions\n{}", limited.join("\n")));
    }

    if !progress_items.is_empty() {
        let items = if progress_items.len() > 10 {
            &progress_items[..10]
        } else {
            &progress_items
        };
        parts.push(format!("## Progress\n{}", items.join("\n")));
    }

    if !recent_errors.is_empty() {
        let limited = if recent_errors.len() > 3 {
            &recent_errors[..3]
        } else {
            &recent_errors
        };
        parts.push(format!("## Recent errors\n{}", limited.join("\n")));
    }

    if !user_msgs.is_empty() {
        parts.push(format!("## User messages\n- {}", user_msgs.join("\n- ")));
    }

    // Spec anchor: if the agent wrote a task_spec.md (spec-first protocol),
    // reference it so a later turn can re-read the full original spec
    // instead of relying on the degraded task summary above.
    if !working_dir.is_empty() {
        let spec_path = std::path::Path::new(working_dir).join(crate::quality::SPEC_FILE);
        if spec_path.is_file() {
            parts.push(format!(
                "## Task spec\n{}\n(re-read for the full original spec / 完整规格见该文件，必要时重读)",
                spec_path.display()
            ));
        }
    }

    parts.join("\n\n")
}

/// Extract text from a text-type content block.
fn extract_text(block: &Value) -> Option<&str> {
    block
        .get("text")
        .and_then(|t| t.get("text"))
        .and_then(|t| t.as_str())
}

/// Extract file paths from a tool input JSON value.
fn extract_paths(input: Option<&Value>) -> Vec<String> {
    let input = match input {
        Some(v) => v,
        None => return Vec::new(),
    };
    let mut paths = Vec::new();
    // Check common path keys
    for key in &["file_path", "path", "file", "target", "directory", "dir"] {
        if let Some(p) = input.get(key).and_then(|v| v.as_str()) {
            paths.push(p.to_string());
        }
    }
    // Also check if input is an array of paths
    if let Some(arr) = input.as_array() {
        for item in arr {
            if let Some(p) = item.as_str() {
                paths.push(p.to_string());
            }
        }
    }
    // Check content field for filename hints
    if let Some(content) = input.get("content").and_then(|v| v.as_str()) {
        // Extract first line as a description
        let first_line = content.lines().next().unwrap_or("");
        if first_line.len() < 100 && !first_line.is_empty() && paths.is_empty() {
            paths.push(format!("({})", truncate_safe(first_line, 60)));
        }
    }
    paths
}

/// Extract todo items from todowrite/todoupdate input.
fn extract_todo_items(input: &Value, items: &mut Vec<String>) {
    if let Some(todos) = input.get("todos").and_then(|v| v.as_array()) {
        for todo in todos {
            let content = todo.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let status = todo.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let mark = match status {
                "completed" | "done" | "ok" => "✅",
                "in_progress" => "🔄",
                _ => "⏳",
            };
            items.push(format!("  {} {}", mark, truncate_safe(content, 80)));
        }
    }
}

/// Brief description of tool input for action summaries.
fn input_desc(input: Option<&Value>) -> String {
    let input = match input {
        Some(v) => v,
        None => return String::new(),
    };
    // Try common description fields
    for key in &[
        "query",
        "command",
        "prompt",
        "message",
        "expression",
        "pattern",
    ] {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            return truncate_safe(v, 60);
        }
    }
    // If it's a simple value, use it
    if let Some(s) = input.as_str() {
        return truncate_safe(s, 60);
    }
    // Fall back to key count
    if let Some(obj) = input.as_object() {
        return format!("{} fields", obj.len());
    }
    String::new()
}

/// Truncate a string to `max_chars` at a UTF-8 char boundary, with an
/// informative suffix showing how much was truncated.
fn truncate_safe(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let t = s.trim();
    if t.len() <= max_bytes {
        return t.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !t.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &t[..end])
}

/// Truncate a compression summary string to `max_chars`, keeping the
/// beginning and end so task context and recent actions are preserved.
/// Returns the original string if it's within the limit.
pub fn truncate_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let keep_head = max_chars * 2 / 3;
    let keep_tail = max_chars - keep_head;
    let head_end = find_char_boundary(text, keep_head);
    let tail_start = find_char_boundary_reverse(text, keep_tail);
    format!(
        "{}\n...[{} chars truncated]...\n{}",
        &text[..head_end],
        text.len().saturating_sub(max_chars),
        &text[tail_start..],
    )
}

fn find_char_boundary(s: &str, max: usize) -> usize {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn find_char_boundary_reverse(s: &str, keep: usize) -> usize {
    let start = s.len().saturating_sub(keep);
    let mut pos = start;
    while pos < s.len() && !s.is_char_boundary(pos) {
        pos += 1;
    }
    pos
}

/// Compression quality metrics for validation.
#[derive(Debug, Clone)]
pub struct CompressionMetrics {
    /// Fraction of chars saved: Δ ≥ 0.85
    pub delta: f64,
    /// Ratio of removed to kept messages: BR ≥ 0.85
    pub balance_ratio: f64,
    /// Messages before compression
    pub before: usize,
    /// Messages after compression
    pub after: usize,
    /// Characters before compression
    pub chars_before: usize,
    /// Characters after compression
    pub chars_after: usize,
}

impl CompressionMetrics {
    pub fn compute(
        chars_before: usize,
        chars_after: usize,
        msg_before: usize,
        msg_after: usize,
    ) -> Self {
        let delta = if chars_before > 0 {
            chars_before.saturating_sub(chars_after) as f64 / chars_before as f64
        } else {
            0.0
        };
        let balance_ratio = if msg_after > 0 {
            msg_before.saturating_sub(msg_after) as f64 / msg_after as f64
        } else {
            0.0
        };
        CompressionMetrics {
            delta,
            balance_ratio,
            before: msg_before,
            after: msg_after,
            chars_before,
            chars_after,
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "Δ={:.0}% BR={:.0}% ({}→{} msgs, {}→{} chars)",
            self.delta * 100.0,
            self.balance_ratio * 100.0,
            self.before,
            self.after,
            self.chars_before,
            self.chars_after,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_measure_usage_empty() {
        let stats = measure_usage(&[]);
        assert_eq!(stats.total_chars, 0);
        assert_eq!(stats.message_count, 0);
    }

    #[test]
    fn test_measure_usage_with_messages() {
        let msgs = vec![
            Message::system("system prompt"),
            Message::user("hello world"),
        ];
        let stats = measure_usage(&msgs);
        assert!(stats.total_chars > 0);
        assert!(stats.system_chars > 0);
        assert!(stats.user_chars > 0);
    }

    #[test]
    fn test_compress_messages_under_budget_no_change() {
        let mut msgs = vec![Message::user("hello")];
        let saved = compress_messages(&mut msgs, 10000, &CompressionConfig::default(), None);
        assert_eq!(saved, 0);
        assert_eq!(msgs.len(), 1);
    }

    #[test]
    fn test_compress_messages_removes_old_messages() {
        let mut msgs: Vec<Message> = (0..40)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(&format!("user message {}", i))
                } else {
                    Message::assistant(&format!("assistant response {}", i))
                }
            })
            .collect();

        // Set min_messages=4 so preserve_pairs=3 + min generates drops
        let mut config = CompressionConfig::default();
        config.min_messages = 4;
        let saved = compress_messages(&mut msgs, 1, &config, None);
        assert!(saved > 0);
    }

    #[test]
    fn test_compress_tool_results_truncates_long_output() {
        let long_text = "A".repeat(10_000);
        let mut msgs = vec![
            Message::user("do something"),
            Message::tool("test_id", long_text.clone()),
        ];

        let mut config = CompressionConfig::default();
        config.tool_result_budget = 500;
        // Small context_win to force compression (budget = 100 * 3 = 300)
        let saved = compress_messages(&mut msgs, 100, &config, None);
        assert!(saved > 0, "compression should save at least 0 chars");
        // Verify the tool result was truncated
        let total_after: usize = msgs
            .iter()
            .map(|m| {
                m.content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text, .. } => text.len(),
                        ContentBlock::ToolResult { content, .. } => match content {
                            oz_core_types::ContentContainer::Text(t) => t.len(),
                            oz_core_types::ContentContainer::Blocks(bs) => bs
                                .iter()
                                .map(|b| match b {
                                    ContentBlock::Text { text, .. } => text.len(),
                                    _ => 0,
                                })
                                .sum(),
                        },
                        _ => 0,
                    })
                    .sum::<usize>()
            })
            .sum::<usize>();
        assert!(
            total_after < 10_000,
            "content should be smaller after compression"
        );
    }

    #[test]
    fn test_compress_messages_preserves_minimum() {
        let mut msgs: Vec<Message> = (0..6)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(&format!("msg {}", i))
                } else {
                    Message::assistant(&format!("resp {}", i))
                }
            })
            .collect();

        let mut config = CompressionConfig::default();
        config.min_messages = msgs.len();
        let _saved = compress_messages(&mut msgs, 1, &config, None);
        // Should not drop below min_messages
        assert_eq!(msgs.len(), 6);
    }

    #[test]
    fn test_compress_reaches_target_not_trigger() {
        // 30 turns with heavy payloads: ~190K chars ≈ 47.5K tokens (chars/4),
        // well over trigger = 32K (80% of 40K window).
        let mut msgs: Vec<Message> = (0..60)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(&format!("user message {i} {}", "x".repeat(3000)))
                } else {
                    Message::assistant(&format!("assistant response {i} {}", "y".repeat(3000)))
                }
            })
            .collect();

        let mut config = CompressionConfig::default();
        config.target_tokens = 8_000;
        config.min_messages = 4;
        let saved = compress_messages(&mut msgs, 40_000, &config, None);
        assert!(saved > 0, "compression should save tokens");

        // Compressed to the TARGET (8K), far below the trigger (32K) —
        // this is the fix for the 170K → 166K shallow thrash.
        let after = measure_usage(&msgs).total_tokens();
        assert!(
            after <= 8_000,
            "after compression {after} should be ≤ target 8K"
        );
        assert!(
            after < 32_000,
            "after compression {after} should be < trigger 32K"
        );
        // Tail turns survive.
        assert!(
            msgs.len() >= 4,
            "expected at least the tail 2 turns, got {}",
            msgs.len()
        );
    }

    #[test]
    fn test_compress_preserves_tail_turns() {
        // 6 turns × ~3K chars ≈ 9K tokens > trigger = 6.4K (80% of 8K
        // window). Target is tiny (1K) but the tail_turns=2 protection
        // must stop the drop at the last 2 user turns.
        let mut msgs: Vec<Message> = (0..12)
            .map(|i| {
                if i % 2 == 0 {
                    Message::user(&format!("user message {i} {}", "x".repeat(3000)))
                } else {
                    Message::assistant(&format!("assistant response {i} {}", "y".repeat(3000)))
                }
            })
            .collect();

        let mut config = CompressionConfig::default();
        config.target_tokens = 1_000;
        config.min_messages = 1;
        let _saved = compress_messages(&mut msgs, 8_000, &config, None);

        // Last two user turns survive: user 8..assistant 11
        assert_eq!(msgs.len(), 4, "expected tail 2 turns, got {}", msgs.len());
        assert!(msgs[0].content_text().contains("user message 8"));
        assert!(msgs[3].content_text().contains("response 11"));
    }

    #[test]
    fn test_compression_pipeline_summary_covers_removed_window() {
        // Mirrors the agent_loop compression block: snapshot → compress →
        // summary_json slice (system offset) → build summary → inject.
        // Verifies the summary covers EXACTLY the removed window, not
        // the head of the conversation (old behavior sliced from index 0,
        // mixing in system prompts and missing the actual drops).
        let mut msgs = vec![Message::system("you are openzen")];
        for i in 0..30 {
            msgs.push(Message::user(&format!(
                "user message {i} {}",
                "x".repeat(3000)
            )));
            msgs.push(Message::assistant(&format!(
                "assistant response {i} {}",
                "y".repeat(3000)
            )));
        }

        let snapshot_before: Vec<serde_json::Value> = msgs
            .iter()
            .filter_map(|m| serde_json::to_value(&m).ok())
            .collect();
        let sys_count = msgs.iter().take_while(|m| m.role == Role::System).count();
        let before_count = msgs.len();

        let mut config = CompressionConfig::default();
        config.target_tokens = 8_000;
        config.min_messages = 2;
        let _saved = compress_messages(&mut msgs, 40_000, &config, None);

        let removed_count = before_count.saturating_sub(msgs.len());
        assert!(removed_count > 0, "expected messages to be removed");

        let start = sys_count.min(snapshot_before.len());
        let end = (sys_count + removed_count).min(snapshot_before.len());
        let summary_json = &snapshot_before[start..end];
        assert_eq!(summary_json.len(), removed_count);

        // The window starts at the first removed message (past system) —
        // "user message 0" — and must NOT contain the system prompt.
        let first_role = summary_json[0]
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("");
        assert_eq!(
            first_role, "user",
            "summary window must start past system prompts"
        );
        assert!(!summary_json[0].to_string().contains("you are openzen"));

        // Template summary built from the removed window reflects real content.
        let template = build_compression_summary(summary_json, "");
        assert!(
            template.contains("user message 0"),
            "template should cover removed window, got: {template}"
        );
        assert!(
            !template.contains("you are openzen"),
            "system prompt must not leak into summary"
        );
    }

    #[test]
    fn test_truncate_content_text_over_budget() {
        let content = oz_core_types::ContentContainer::Text("A".repeat(1000));
        let truncated = truncate_content(&content, 100);
        if let oz_core_types::ContentContainer::Text(t) = &truncated {
            assert!(t.len() < 1000);
            assert!(t.contains("truncated"));
        } else {
            panic!("expected Text variant");
        }
    }

    #[test]
    fn test_compress_tool_results_handles_blocks() {
        let blocks = vec![
            ContentBlock::text("A".repeat(5000)),
            ContentBlock::text("B".repeat(5000)),
        ];
        let content = oz_core_types::ContentContainer::Blocks(blocks);
        let truncated = truncate_content(&content, 1000);
        if let oz_core_types::ContentContainer::Blocks(bs) = &truncated {
            let total: usize = bs.iter().map(|b| content_block_len(b)).sum();
            // Should be at or under budget (with some overhead for truncation markers)
            assert!(total <= 1500, "truncated content too long: {total} > 1500");
            assert!(bs.len() <= 3, "expected at most 3 blocks, got {}", bs.len());
        } else {
            panic!("expected Blocks variant");
        }
    }

    #[test]
    fn test_compression_config_default() {
        let config = CompressionConfig::default();
        assert_eq!(config.tool_result_budget, 3000);
        assert_eq!(config.old_assistant_budget, 2000);
        assert_eq!(config.old_user_budget, 5000);
        assert!(config.enable_summarization);
    }

    // ── build_compression_summary tests ──

    #[test]
    fn test_build_summary_extracts_preview_from_user() {
        let removed = vec![
            serde_json::json!({
                "role": "system",
                "content": [{"text": {"text": "You are a helpful assistant"}}]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"text": {"text": "Please write a chess engine in Rust"}}]
            }),
        ];
        let summary = build_compression_summary(&removed, "");
        // Should NOT use system prompt as preview
        assert!(
            !summary.contains("You are a helpful assistant"),
            "should skip system message preview, got: {summary}"
        );
        // Should use user message as preview
        assert!(
            summary.contains("chess engine"),
            "should show user message preview, got: {summary}"
        );
    }

    #[test]
    fn test_build_summary_counts_tools_from_tool_use_blocks() {
        let removed = vec![serde_json::json!({
            "role": "assistant",
            "content": [
                {"text": {"text": "I'll read the file"}},
                {"tool_use": {"id": "call_1", "name": "read", "input": {"file_path": "/tmp/x"}}},
                {"tool_use": {"id": "call_2", "name": "write", "input": {"file_path": "/tmp/y"}}},
            ]
        })];
        let summary = build_compression_summary(&removed, "");
        assert!(
            summary.contains("read"),
            "should list read tool, got: {summary}"
        );
        assert!(
            summary.contains("write"),
            "should list write tool, got: {summary}"
        );
    }

    #[test]
    fn test_build_summary_skips_compression_summary_messages() {
        let removed = vec![
            serde_json::json!({
                "role": "system",
                "content": [{"text": {"text": "[Compression summary]: old summary"}}]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"text": {"text": "real task description here"}}]
            }),
        ];
        let summary = build_compression_summary(&removed, "");
        assert!(
            !summary.contains("[Compression summary]"),
            "should skip compression summary text, got: {summary}"
        );
        // System role should not contribute to user/assistant counts
        assert!(
            !summary.contains("0 user"),
            "system != user; preview should be user msg"
        );
    }

    #[test]
    fn test_build_summary_handles_cjk_safe_truncation() {
        let chinese = "你好世界".repeat(40);
        let removed = vec![serde_json::json!({
            "role": "user",
            "content": [{"text": {"text": chinese}}]
        })];
        let summary = build_compression_summary(&removed, "");
        // New format uses "Compressed:" prefix
        assert!(
            summary.contains("Compressed: 1 messages removed"),
            "unexpected format: {summary}"
        );
        // Task should be CJK-text truncated safely
        assert!(summary.contains("## Task"), "should have Task section");
        // Should not panic on CJK boundary
    }

    #[test]
    fn test_trim_message_content_cjk_no_panic() {
        // Regression: compress.rs used `String::truncate(remaining)` where
        // `remaining` is a char budget, not a byte boundary. Multi-byte
        // UTF-8 (CJK = 3 bytes/char) landed mid-character and panicked
        // with `assertion failed: self.is_char_boundary(new_len)`,
        // killing the agent loop mid-task (openzen-crash.log 12:47:37).
        let mut msg = Message::user(&"你好世界".repeat(3000)); // 36_000 bytes
        let config = CompressionConfig::default();
        let stats = UsageStats::default(); // token_ratio=0 → char_budget = tokens*4
        let saved = trim_message_content(&mut msg, &config, &stats);
        // Must have truncated something and must be valid UTF-8 (no panic).
        assert!(saved > 0, "expected content to be trimmed");
        for block in &msg.content {
            if let ContentBlock::Text { text, .. } = block {
                assert!(text.len() < 36_000, "content should shrink");
                assert!(
                    text.is_char_boundary(text.len()),
                    "must end on char boundary"
                );
                let _ = text.chars().count(); // iterates fine only if valid UTF-8
            }
        }
    }

    // ── Full pipeline tests ──

    #[test]
    fn test_full_compression_pipeline_extracts_correct_removed_msgs() {
        // 23 messages with decent content to exceed token trigger
        let filler = "x".repeat(80); // pad messages to exceed small trigger
        let mut msgs: Vec<Message> = vec![Message::system(&format!(
            "# Role: Assistant\nChinese system prompt text here...\n{filler}"
        ))];
        for i in 0..11 {
            msgs.push(Message::user(&format!("task step {} - {filler}", i)));
            msgs.push(Message::assistant_with_blocks(vec![
                ContentBlock::text(&format!("working on step {} - {filler}", i)),
                ContentBlock::tool_use(
                    &format!("call_{}", i),
                    "read",
                    serde_json::json!({"path": &format!("/tmp/file{}", i)}),
                ),
            ]));
        }

        let before_count = msgs.len();
        let snapshot: Vec<serde_json::Value> = msgs
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();

        // context_win=50 → trigger=40 tokens. With ~3000 chars → ~750 tokens > 40.
        let config = CompressionConfig::default();
        let saved = compress_messages(&mut msgs, 50, &config, None);
        let removed_count = before_count.saturating_sub(msgs.len());

        assert!(
            saved > 0 || removed_count > 0,
            "compression failed: before={before_count}, after={}, saved={saved}",
            msgs.len()
        );

        if removed_count > 0 {
            let removed_json: Vec<serde_json::Value> =
                snapshot.into_iter().take(removed_count).collect();
            let summary = build_compression_summary(&removed_json, "");

            assert!(
                !summary.contains("# Role:"),
                "system prompt leaked into compression summary: {summary}"
            );
            // New format has structured sections
            assert!(
                summary.contains("Compressed:"),
                "should have compressed header: {summary}"
            );
            assert!(
                summary.contains("## Task"),
                "should have Task section: {summary}"
            );
            assert!(
                summary.contains("## Files"),
                "should have Files section: {summary}"
            );
            assert!(
                summary.contains("## Actions"),
                "should have Actions section: {summary}"
            );
        }
    }

    #[test]
    fn test_phase3_remeasures_after_trimming() {
        // 31 messages with padding to exceed token trigger
        let filler = "x".repeat(80);
        let mut msgs: Vec<Message> = vec![Message::system(&format!("short sys {filler}"))];
        for i in 0..15 {
            msgs.push(Message::user(&format!("task step {} - {filler}", i)));
            msgs.push(Message::assistant(&format!("response {} - {filler}", i)));
        }

        let before = msgs.len();
        let config = CompressionConfig::default();
        // context_win=50 → trigger=40 tokens. With padding, tokens >> 40.
        let saved = compress_messages(&mut msgs, 50, &config, None);
        let after = msgs.len();

        assert!(
            after < before,
            "should remove messages: {before} -> {after} (saved {saved} chars)"
        );
        assert!(
            after >= config.min_messages,
            "should not drop below min_messages: {after} < {min}",
            min = config.min_messages
        );
    }

    // ── build_compression_prompt tests ──

    #[test]
    fn test_build_prompt_extracts_full_text() {
        let removed = vec![
            serde_json::json!({
                "role": "user",
                "content": [{"text": {"text": "Build a chess engine in Rust"}}]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"text": {"text": "I'll create the files now"}},
                    {"tool_use": {"id": "c1", "name": "write", "input": {"file_path": "/tmp/Cargo.toml"}}},
                ]
            }),
        ];
        let prompt = build_compression_prompt(&removed);
        assert!(prompt.contains("[user]: Build a chess engine"));
        assert!(prompt.contains("[assistant]: I'll create the files"));
        assert!(prompt.contains("<tool_use:write>"));
    }

    #[test]
    fn test_build_prompt_skips_compression_summaries() {
        let removed = vec![
            serde_json::json!({
                "role": "system",
                "content": [{"text": {"text": "[Compression summary]: old summary"}}]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"text": {"text": "real task"}}]
            }),
        ];
        let prompt = build_compression_prompt(&removed);
        assert!(!prompt.contains("Compression summary"));
        assert!(prompt.contains("real task"));
    }

    #[test]
    fn test_build_prompt_includes_tool_results() {
        let removed = vec![serde_json::json!({
            "role": "user",
            "content": [{"tool_result": {"tool_use_id": "c1", "content": "file written successfully"}}]
        })];
        let prompt = build_compression_prompt(&removed);
        assert!(prompt.contains("[tool_result]: file written successfully"));
    }

    #[test]
    fn test_build_prompt_truncates_long_tool_input() {
        let long_input = serde_json::json!({"content": "x".repeat(5000)});
        let removed = vec![serde_json::json!({
            "role": "assistant",
            "content": [
                {"tool_use": {"id": "c1", "name": "write", "input": long_input}},
            ]
        })];
        let prompt = build_compression_prompt(&removed);
        // Tool input should be truncated, not 5000 chars in prompt
        let after_tool_use = prompt.split("<tool_use:write>").nth(1).unwrap_or("");
        assert!(
            after_tool_use.len() < 500,
            "tool input not truncated: {} chars",
            after_tool_use.len()
        );
    }
}
