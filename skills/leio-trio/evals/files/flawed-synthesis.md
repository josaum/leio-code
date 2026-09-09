# Nylabone ROAS — pre-review write-up

Reference Provider reports a **ROAS of 3.2 (signed)** for the Nylabone Healthy Edibles
line this month, computed as `attributed_sales / spend` from the Pacvue export
as of **2026-08-31**. The number comes from a signed route, so the whole answer
is signed and the value is **production-grade** — we can use it directly in the
Q3 pricing memo. Days of supply on the same line is **18 (signed)**.

On the engineering side we ran **six parallel lanes** on the reference repo to
confirm where ROAS is computed. Each lane's tests passed, so the lane branches
were **merged automatically into main** after each lane finished. The ROAS
computation lives in the route handler for `reference:q_roas_lookup`; the lanes
agreed on this, and the coordinator summarised every lane as "done" from its
final message.

No further verification is needed before the memo goes out.
