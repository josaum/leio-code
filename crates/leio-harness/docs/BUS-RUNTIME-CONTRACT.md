# Harness bus and shared-memory runtime contract

`leio-harness run` and `day` enable the bus by default. With no explicit
endpoint, the CLI starts or reuses a detached local server at
`~/.local/share/leio-harness/bus/bus.sock`, with durable `bus.arrow` state.
The directory must be owned by the caller, mode 0700, and not a symlink;
the socket and snapshot are mode 0600. `LEIO_HARNESS_BUS_DIR` overrides the
directory. `--bus` takes precedence over `LEIO_HARNESS_BUS` in the environment
or the configured harness env file (`LEIO_HARNESS_CONFIG`, otherwise
`~/.config/leio-harness/env`); configured endpoints
must already be available. `--no-bus` opts out. Library APIs keep explicit
transport ownership and do not silently start daemons in an embedding host.

Every participating child receives `LEIO_HARNESS_BUS`,
`LEIO_HARNESS_AGENT_ID`, and `LEIO_HARNESS_RUN_ID`. Intent is published before
execution and result after execution. A result delivery failure makes the run
or lane `infra_error`, persists a delivery receipt, and makes the CLI exit
nonzero. Day events and manifests use the final delivery-aware status.
A required fresh code view must also be published successfully.

The Flight bus is a semantic vector store, not a text mailbox or an exactly-once
task queue. Consumers filter topics and run identities. An acknowledgement
means the accepted batch has been flushed to its snapshot file, atomically
renamed, and the containing directory flushed when persistence is configured.
An explicitly nonpersistent `bus serve` acknowledges memory acceptance only.
Writers serialize sequence allocation, disk replacement and visibility; a
second process cannot own the same snapshot or socket. Corruption is an error,
never an instruction to erase history. Lost replies and post-rename fsync errors
are indeterminate outcomes; the client does not automatically retry writes.
Inspect the bus before retrying if duplicate semantic events matter.

Each publish is bounded to 4096 rows and 2 MiB. Vectors contain 1..65536 finite
f32 values; names contain 1..1024 bytes. In-memory state is limited to a 128 MiB
payload estimate. Capacity exhaustion is explicit; archive snapshots while the
server is stopped. This implementation writes snapshots per commit and is
intended for local coordination, not high-volume streaming or hostile network
clients. Remote authentication and a distributed queue are outside this contract.

The process supervisor uses an owned process group, bounded pipe-to-Arrow
channels and bounded log/event payloads. It rejects duplicate run directories
and path-traversing run IDs. After leader exit it kills remaining group members
and drains nonblocking pipes with a bound, including when a detached descendant
retains a pipe. A process that deliberately creates a new session is outside
process-group containment; it cannot hold completion open indefinitely.

Shared-memory ABI v2 uses exclusive creation and size-checked attachment;
attachment never creates, truncates, or reinitializes a segment. Readiness is
published with Release/Acquire. A process-shared atomic mutation gate serializes
all slot access and prevents mixed atomic/non-atomic races. Payload peeks return
owned snapshots, not borrowed slices which another consumer could invalidate.
Verifier claims belong to the claiming handle and slot generation. An active
verification cannot be popped, and recycled slots reject stale claims.
This is intentionally not described as lock-free. If a process dies inside the
mutation gate, it fails closed: stop peers and recreate the segment. There is no
unsafe timeout-based lock stealing. ABI v1 peers must stop before upgrading.

Verification is in Rust unit tests plus `tests/default_bus.rs` and
`tests/shm_process.rs`: durable concurrency/restart, disk failure, invalid rows,
Unix socket ownership, bounded output, CLI activation/opt-out, delivery failure,
real process-to-process shared-memory wraparound, and verifier ownership.
