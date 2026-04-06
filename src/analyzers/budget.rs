// Budget observer: tracks thought budget and flags under/overthinking.
// Implements Observer trait. Returns Observation::Budget { used, max, category }.
//
// Budget tiers (from config, keyed by thinking mode):
//   "minimal"  -- 2-3 thoughts (simple modes, few components)
//   "standard" -- 3-5 thoughts (debugging, performance, moderate complexity)
//   "deep"     -- 5-8 thoughts (architecture, scaling, 5+ components)
//
// Budget derived from: mode config + affected_components.len() + active branch count.
//
// Alerts:
//   UNDERTHINKING -- next_thought_needed=false before reaching budget minimum.
//   OVERTHINKING  -- past 2x budget with no new branches/revisions in last 3 thoughts.
