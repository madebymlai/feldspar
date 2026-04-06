// Trace review: external trust scoring via OpenRouter HTTP call from Rust binary.
// Runs automatically after every trace completes. One API call, ~1 second.
// Model: openai/gpt-oss-20b:nitro (cheap, fast).
//
// Prompt (locked in, tested):
//   System: "You are a reasoning quality judge. You will be given a thinking
//   trace and its thinking mode. Scale your expectations to the complexity
//   -- broader modes demand deeper reasoning. On a scale of 0-10, how much
//   would you trust the conclusion enough to act on it? Respond with ONLY
//   a JSON object: {"trust": <number>, "reason": "<one sentence>"}"
//
//   User: "Mode: {thinking_mode}\n---\n{formatted thoughts}"
//
// Mode-aware: architecture/3 thoughts scores low, debugging/3 thoughts scores high.
// Trust score is the ML target variable.
//
// Best-effort: HTTP failure → log, skip, ML gets no outcome for this trace.
// API key from env var named in config (trace_review.api_key_env).
// Model from config (trace_review.model).
// Entry point: TraceReviewer::review(trace: &[ThoughtRecord], mode: Option<&str>) -> Option<TrustScore>
