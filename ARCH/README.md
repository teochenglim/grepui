# ARCH

Architecture and the reasons behind it. Read in order.

| File | Content |
|---|---|
| [00_requirements_traceability.md](00_requirements_traceability.md) | R-IDs. Chain: requirement → AC (in `RELEASE/`) → ≥2 tests. Enforced by `make ac-map` |
| [01_architecture.md](01_architecture.md) | Components, tenancy, trust boundary, request flow |
| [02_reference_uis.md](02_reference_uis.md) | Reference products (Quickwit and others): what we take and what we do not |
| [03_greptimedb_mapping.md](03_greptimedb_mapping.md) | UI features → GreptimeDB SQL/APIs, rollups, what is unverified |
| [04_testing_and_ac_map.md](04_testing_and_ac_map.md) | Test layers, naming rules, the AC gate |
| [05_tech_stack_and_release.md](05_tech_stack_and_release.md) | Dependencies and why, versioning, CI and release pipeline |
| [06_positioning.md](06_positioning.md) | Goal (parity plus enterprise tier), design choices D1–D9, differences, non-goals |
| [07_parity_matrix.md](07_parity_matrix.md) | Feature-by-feature comparison with the reference platform, evidence-tagged |
| [architecture.mmd](architecture.mmd) | Mermaid source of the diagram |
