// Feldspar MCP server: single daemon per project, MCP over HTTP on localhost.
// Multiple Claude Code sessions connect concurrently. One ML model, one DB, always fresh.
//
// Tokio runtime. HTTP requests handled async. Analyzer pipeline + ML predict are sync (microseconds).
// Background work (DB writes, trace review, ML training) fire-and-forget via tokio::spawn.
//
// Startup: load config → open SQLite (WAL) → prune → bulk train ML → load model file → start HTTP server.
// Shutdown: stop connections → flush traces → save model → close DB.
//
// One MCP tool: sequentialthinking. Trace review is internal (HTTP to OpenRouter).
// Error handling: never crash. Bad input → error response. Analyzer panic → catch_unwind + skip.

mod analyzers;
mod config;
mod db;
mod ml;
mod pruning;
mod thought;
mod trace_review;
mod warnings;
