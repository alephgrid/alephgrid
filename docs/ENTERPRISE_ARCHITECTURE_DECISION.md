# Enterprise architecture decision: workbook-first, policy-managed compute

Date: 26 October 2026.
Status: recommended direction following a two-agent debate; not an implemented architecture or a claim of production readiness.
Scope: design recommendation only. The implementation checklist was not modified.

## Chosen direction

Build an extensible workbook platform with automatic compute placement. Users open durable workbooks; the platform starts or selects compatible runtime capacity under administrator-defined policies. Explicit profiles and dedicated compute remain advanced capabilities.

Keep organizations/spaces, durable data, reproducible runtime definitions and live compute allocations separate. Do not make a machine or a user's session the owner of shared data.

## Debate and resolution

Two agents exchanged proposals and rebuttals directly:

| Position | Strongest argument | Resolution |
|---|---|---|
| Explicit JupyterHub-style environments | Reproducible dependencies, clear resource choices and workload isolation are essential enterprise capabilities | Retain immutable runtime specifications, approved profiles and optional dedicated allocations |
| Automatic workbook placement | Spreadsheet collaboration should survive its creator's session; mandatory instance selection adds friction and cost | Make opening a workbook the default action and keep placement behind the platform |

Reject mandatory one-instance-per-user or one-instance-per-space architecture. Also reject an unrestricted shared process for mutually untrusted workbooks. Shared infrastructure and shared security boundaries are different decisions.

## Core objects

- **Organization / Space:** identity, membership, permissions, quotas and policy.
- **Workbook:** stable identity, durable tables/sheets, revisions and pinned dependencies.
- **RuntimeSpec:** immutable, versioned runtime image, engine/package versions and required capabilities.
- **ComputePolicy:** allowed profiles, resource budgets, isolation class, idle behavior and permitted providers.
- **RuntimeAllocation:** replaceable live process/container/pod with endpoint, health and ownership information.

The separation permits a workbook to move or restart without changing its URL, ownership or function versions. It does not promise live migration or uninterrupted resizing.

## Default experience

1. User opens a workbook.
2. Control services check authorization, policy and quota.
3. The platform routes to the active owner or starts compatible capacity.
4. The UI shows meaningful waiting/startup/failure states.
5. Collaborators connect to the same authoritative workbook owner.
6. Idle shutdown checkpoints/persists work before releasing compute; durable storage remains.

Expose Standard/Large/Dedicated profiles only where useful and authorized. Initial resizing is a controlled stop-and-restart, with collaborators informed. Do not silently increase resource spend.

## Extension boundaries for an open ecosystem

Start with narrow internal interfaces, exercise them with real implementations, then publish versioned external contracts. Do not promise a generic plugin framework before its semantics are tested.

| Contract | Responsibility | What it cannot bypass |
|---|---|---|
| RuntimeProvider | Ensure/start, inspect, drain and stop allocations; report capabilities and readiness | Authorization, quotas, ownership and durability rules |
| DataConnector | Discover/import/refresh authorized data with explicit types and source versions | Credential scopes and data-access policy |
| FunctionRuntime | Execute versioned typed functions with bounded inputs/outputs and cancellation | Execution isolation and revision checks |
| ArtifactStore | Persist/retrieve versioned recovery artifacts under a defined consistency contract | Ownership checks or acknowledgement durability |
| Public API / events | Integrate clients, automation and audit consumers | Server-side permissions and protocol compatibility |

Provider requests must be idempotent, carry operation identifiers, and support reconciliation after controller restart. Establish timeouts, cancellation, error schemas and version negotiation. Run the same conformance suite against each provider/runtime implementation.

Untrusted executable extensions belong outside the core server with explicit capabilities. Initially, deployment administrators approve/install extensions. Examples of future community work include new deployment adapters, connectors, scientific function packages, visualization integrations and alternative clients.

## Storage, ownership and failure behavior

One authoritative mutable workbook owner remains the basic model. Node pools may be shared; sharing an OS process across workbooks requires an explicitly accepted trust and resource boundary. Python execution remains separate.

**A lease expiration does not fence direct DuckDB file writes.** The first Kubernetes adapter must verify the old owner's termination and exclusive storage handoff before allowing a replacement writer. If it cannot establish exclusivity, it must refuse writes and require recovery rather than risk divergent databases. A Kubernetes namespace or a storage access-mode label alone is not proof of safe application ownership.

Persist acknowledgements according to a documented durability contract. A copy of a database file without its required consistency procedure is not a sufficient backup strategy. Define crash recovery and restore behavior before advertising availability guarantees. Cross-host recovery, cross-zone recovery and remote durable command replay are separate tested capabilities.

## Practical delivery boundaries

1. **Dependable standalone product:** existing engine/UI, transactional workflows, permissions, revision semantics, backup/restore and bounded work. Internal lifecycle interfaces run on one host; no mandatory cloud services.
2. **First enterprise deployment:** control metadata and routing separated logically from workers; local/container provider proven; approved profiles, audit, quotas and observable lifecycle. Components can still ship together where practical.
3. **Kubernetes provider:** Helm deployment, persistent storage, automatic placement, controlled shutdown/restart and conservative exclusive-owner recovery. AWS EKS/Karpenter is one infrastructure option, not a core product dependency.
4. **Validated ecosystem:** second provider proves portability, published contracts and conformance tests, documented compatibility policy and contributor governance.
5. **Later scale:** workload balancing and stronger failover after their storage/fencing designs are verified. Distributed execution inside one workbook remains separate scope.

Do not build a bespoke cloud scheduler: delegate pod/node placement to the selected infrastructure and retain workbook policy/lifecycle in Alephgrid. Do not make a JupyterHub fork the core; consider an integration for existing JupyterHub installations after the standalone contract is stable.

## Evidence required before calling it industry-grade

- Complete browser workflows with exact results and persisted state.
- Cross-space authorization tests across HTTP, WebSocket, SQL, exports, connectors and functions.
- Atomic mutation, crash/restart, consistent backup/restore and upgrade verification.
- Concurrent edits, retries, missed events and reconnects follow a documented revision/conflict contract.
- Resource exhaustion and Python-runtime failure cannot escape the declared isolation boundary.
- Allocation retries and controller restart do not spawn conflicting owners or orphan unbounded resources.
- Lost workers/network partitions fail closed where exclusivity cannot be proven.
- Measured capacity, recovery objectives, observability, compatibility and operational runbooks.
- Extension conformance tests and realistic upgrade/deprecation policy.

Unresolved engineering choices include the exact runtime isolation implementation, storage handoff protocol, resource profiles and performance limits. Resolve these with targeted prototypes and failure tests; architectural consensus does not establish them as solved.

## Primary references

- [JupyterHub spawner interfaces](https://jupyterhub.readthedocs.io/en/stable/reference/spawners.html): narrow lifecycle abstractions enable deployment extensions.
- [JupyterHub environment profiles](https://z2jh.jupyter.org/en/4.4.3/jupyterhub/customizing/user-environment.html): selectable administrator-defined environments.
- [Kubernetes multi-tenancy](https://kubernetes.io/docs/concepts/security/multi-tenancy/): tenant isolation requires more than grouping resources.
- [AWS JupyterHub on EKS blueprint](https://aws.amazon.com/blogs/containers/building-multi-tenant-jupyterhub-platforms-on-amazon-eks/): infrastructure reference, not a Alephgrid implementation.
