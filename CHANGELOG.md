# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-05-21

### Added
- Initial public release of ai-motionlens.
- Workspace crates: `ai-motionlens-core`, `ai-motionlens-mcp`, `ai-motionlens` (CLI).
- 33 MCP tools across session / episode + clock / trigger / frame / library / motion / timeline / evidence / scroll / recipes namespaces.
- **Animation Evidence Gate** (`motion.verify`): one MCP call running session.launch → episode → set_intent → triggers → capture_series → contact_sheet → video_export → assess → coverage scoring, with `MotionVerifyReport` on disk and pass/fail bar.
- `motion.audit_required` and `motion.suggest_intent` upstream tools.
- `MotionVerifyReport.diagnosis_hints` with kebab-case root-cause codes and `suggested_probe`.
- `AnimationAssessment` extensions: `per_target_easing`, `stagger_uniformity`, `overshoot_events`, `runtime_jank_events`.
- `Trigger.input_mode` with `js` / `cdp` paths.
- `ExternalStatePolicy` for deterministic replay (`random` / `storage` / `timezone` / `locale` / `user_agent` pins).
- `animation-qa` sub-agent (`agents/animation-qa.md`).
- `ai-motionlens verify` / `gate-check` CLI for CI integration.
- Skill package (`skill/SKILL.md` + references) for AI harnesses.
- E2E Python harness with 57 tests over 34 HTML fixtures (`e2e/run.sh`).
