// Pruning: keeps SQLite lean, runs on startup + configurable interval.
// Traces without outcome older than N days: delete (no ML value).
// Low-quality traces (<3 thoughts, no branches, no warnings) older than N/2 days: delete.
// Traces with outcomes: retain longer (configurable), they're training data.
// Patterns with use_count=1 older than N days: delete.
// Never blocks thought processing. Runs async on worker.
