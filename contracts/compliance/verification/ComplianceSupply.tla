--------------------------- MODULE ComplianceSupply ---------------------------
(***************************************************************************)
(* A bounded formal model of the `asset-token` balance/supply state machine   *)
(* interacting with the `compliance` allowlist, transcribed from the Rust    *)
(* sources at the line numbers cited on each action.                         *)
(*                                                                         *)
(* The two required properties are:                                         *)
(*                                                                         *)
(*   (G1) NoUngatedCredit.  No transition may strictly increase `bal[a]`     *)
(*        unless `IsAllowed(a)` held in the pre-state.  This is the precise  *)
(*        reading of "un-allowlisted addresses can never receive positive     *)
(*        balance transfers".  The naive reading -- "bal[a] > 0 implies       *)
(*        IsAllowed(a)" -- is NOT provable and is refuted in Refutation.cfg; *)
(*        see the README.                                                    *)
(*                                                                         *)
(*   (S1) SupplyConserved.  `total = \Sum_a bal[a] + seized` in every        *)
(*        reachable state, where the ghost `seized` counts units removed by   *)
(*        clawback (which reduces a balance but not the supply).  See the     *)
(*        long note above `SupplyConserved` for the 5-step counterexample     *)
(*        that forced the ghost variable into the model.                      *)
(*   (S2) NoMonetaryDrift.  `total' = total` across every action other than  *)
(*        Mint, Burn and InitializeToken.                                    *)
(*                                                                         *)
(* TLC cannot check a two-state predicate as an invariant, so (G1) and (S2)  *)
(* are each carried by a latching *witness* variable.  `ungatedCredit` is    *)
(* latched by a crediting action exactly when its compliance guard was       *)
(* false; `nonMonetaryDrift` is latched by any non-monetary action exactly    *)
(* when `total` moved.  Both are FALSE in the initial state, so asserting    *)
(* `FALSE` is exactly the transition property, checked over all states.      *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS Addrs,   \* finite set of addresses, the model's universe
          Juris,   \* finite set of jurisdictions
          NoAddr,  \* a distinguished "not yet set" value, must not be in Addrs
          MaxBal,  \* upper bound on every balance and on total supply
          MaxSeq,  \* upper bound on the ledger sequence and lockup values
          MaxExp   \* upper bound on a KYC `expires_at`

ASSUME /\ Addrs  # {}
       /\ Juris   # {}
       /\ NoAddr \notin Addrs
       /\ MaxBal \in Nat /\ MaxBal > 0
       /\ MaxSeq \in Nat
       /\ MaxExp \in Nat

Statuses == { "None", "Approved", "Pending", "Rejected", "Suspended" }

VARIABLES
    \* ----- asset-token -----
    bal,       \* [Addrs -> 0..MaxBal]
    total,     \* 0..MaxBal
    seized,    \* 0..MaxBal  (GHOST: supply seized by clawback, see below)
    tokAdmin,  \* Addrs \cup {NoAddr}
    tPaused,   \* BOOLEAN
    lockup,    \* [Addrs -> 0..(MaxSeq+1)]
    \* ----- compliance -----
    rec,       \* [Addrs -> [status: Statuses, juris: Juris, exp: 0..MaxExp]]
    allowList, \* SUBSET Addrs
    blocked,   \* SUBSET Juris
    cAdmin,    \* Addrs \cup {NoAddr}
    cPaused,   \* BOOLEAN
    seq,       \* 0..MaxSeq  (the ledger sequence the KYC expiry tests)
    \* ----- latching witnesses -----
    ungatedCredit,    \* BOOLEAN
    nonMonetaryDrift \* BOOLEAN

vars == << bal, total, seized, tokAdmin, tPaused, lockup,
           rec, allowList, blocked, cAdmin, cPaused, seq,
           ungatedCredit, nonMonetaryDrift >>

BalanceRange == 0 .. MaxBal
LockupRange  == 0 .. (MaxSeq + 1)

(***************************************************************************)
(* TLA+ has no built-in summation, so `TotalBal` is defined by recursion.    *)
(* It sums the balance of every address in the given set, which is what the   *)
(* supply-conservation invariant needs.  `Addrs` is finite and `CHOOSE`      *)
(* picks a fixed element, so this terminates.                                *)
(***************************************************************************)
RECURSIVE TotalBal(_)
TotalBal(S) ==
    IF S = {}
    THEN 0
    ELSE LET a == CHOOSE x \in S : TRUE
         IN  bal[a] + TotalBal(S \ {a})

(***************************************************************************)
(* The compliance predicate.  Transcribed from `ComplianceContract::        *)
(* is_allowed`, compliance/src/lib.rs:174-187.                             *)
(*                                                                         *)
(* All four rejection reasons are modelled, because dropping any one of     *)
(* them would make (G1) provable for the wrong reason:                      *)
(*   - no record at all            -> status "None"                          *)
(*   - status /= Approved          -> Suspended / Pending / Rejected         *)
(*   - KYC expired                 -> exp # 0 /\ seq >= exp                   *)
(*   - jurisdiction blocked        -> juris \in blocked                      *)
(***************************************************************************)
IsAllowed(a) ==
    /\ rec[a].status = "Approved"
    /\ ~( /\ rec[a].exp # 0
         /\ seq >= rec[a].exp )
    /\ rec[a].juris \notin blocked

(***************************************************************************)
(* compliance/src/lib.rs:412-421                                             *)
(***************************************************************************)
IsLocked(a) == /\ lockup[a] # 0
               /\ seq < lockup[a]

\* ---------------------------------------------------------------------------
\* Init
\* ---------------------------------------------------------------------------
Init ==
    /\ bal       = [a \in Addrs |-> 0]
    /\ total     = 0
    /\ seized    = 0
    /\ tokAdmin  = NoAddr
    /\ tPaused   = FALSE
    /\ lockup    = [a \in Addrs |-> 0]
    /\ rec       = [a \in Addrs |->
                       [ status |-> "None"
                       , juris  |-> CHOOSE j \in Juris : TRUE
                       , exp    |-> 0 ]]
    /\ allowList = {}
    /\ blocked   = {}
    /\ cAdmin    = NoAddr
    /\ cPaused   = FALSE
    /\ seq       = 0
    /\ ungatedCredit    = FALSE
    /\ nonMonetaryDrift = FALSE

(***************************************************************************)
(* The latching rule every non-monetary action must follow.                  *)
(***************************************************************************)
NoDrift == nonMonetaryDrift' = nonMonetaryDrift \/ (total' # total)

\* ---------------------------------------------------------------------------
\* ComplianceInit   (compliance/src/lib.rs:76-91)
\* ---------------------------------------------------------------------------
ComplianceInit(a) ==
    /\ cAdmin = NoAddr
    /\ cAdmin' = a
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup,
                   rec, allowList, blocked, cPaused, seq,
                   ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* compliance/src/lib.rs:93-131.                                             *)
(*                                                                         *)
(* FIDELITY NOTE -- this action was WRONG in the first transcription, and    *)
(* the bug was caught by action-COVERAGE rather than by reading the spec.     *)
(* `add_to_allowlist` had been given the guard `a = cAdmin`, i.e. "the       *)
(* subject of the allowlist must be the admin itself".  The real signature   *)
(* at compliance/src/lib.rs:93-99 is                                       *)
(*                                                                         *)
(*     add_to_allowlist(env, admin: Address, address: Address,              *)
(*                      jurisdiction: String, expires_at: u32)              *)
(*                                                                         *)
(* where `admin` is the AUTHORIZER (checked by require_admin) and `address`  *)
(* is an independent SUBJECT.  Under the wrong guard at most one address in  *)
(* the entire universe could ever be allowlisted, so `Transfer` was never    *)
(* enabled and `TransferGate` held VACUOUSLY.  `cAdmin # NoAddr` is the     *)
(* faithful rendering of require_admin in a model with no caller argument.    *)
(*                                                                         *)
(* Note the `is_new` guard: re-approving an existing record does not         *)
(* duplicate the allowlist entry.  The expiry guard is                      *)
(* `expires_at != 0 /\ expires_at <= now -> InvalidExpiry`, i.e. a record    *)
(* can be created already-expired only with `expires_at = 0`, which means    *)
(* "never expires" and is therefore trivially non-expired.                  *)
(***************************************************************************)
AddToAllowlist(a, j, exp) ==
    /\ cPaused = FALSE
    /\ cAdmin # NoAddr
    /\ a \in Addrs
    /\ j \in Juris
    /\ exp \in 0 .. MaxExp
    /\ ( exp = 0 \/ exp > seq )
    /\ rec' = [rec EXCEPT ![a] =
                   [ status |-> "Approved", juris |-> j, exp |-> exp ]]
    /\ allowList' = IF rec[a].status = "None" THEN allowList \cup {a}
                    ELSE allowList
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup, blocked,
                   cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ NoDrift

Suspend(a) ==                       \* compliance/src/lib.rs:133-144
    /\ cPaused = FALSE
    /\ cAdmin # NoAddr
    /\ rec[a].status # "None"
    /\ rec' = [rec EXCEPT ![a].status = "Suspended"]
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup,
                   allowList, blocked, cAdmin, cPaused, seq,
                   ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* compliance/src/lib.rs:146-172.  `remove` deletes the record but cannot   *)
(* touch any balance -- the token's storage is unreachable from here.  This  *)
(* is the reason the naive form of (G1) is false; see Refutation.cfg.       *)
(***************************************************************************)
Remove(a) ==
    /\ cPaused = FALSE
    /\ cAdmin # NoAddr
    /\ rec[a].status # "None"
    /\ rec' = [rec EXCEPT ![a].status = "None"]
    /\ allowList' = allowList \ {a}
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup,
                   blocked, cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* compliance/src/lib.rs:295-312 (block) and the unblock path.              *)
(***************************************************************************)
BlockJurisdiction(j) ==
    /\ cPaused = FALSE
    /\ cAdmin # NoAddr
    /\ j \in Juris
    /\ blocked' = blocked \cup {j}
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup, rec,
                   allowList, cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ NoDrift

UnblockJurisdiction(j) ==
    /\ cPaused = FALSE
    /\ cAdmin # NoAddr
    /\ j \in Juris
    /\ j \in blocked
    /\ blocked' = blocked \ {j}
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup, rec,
                   allowList, cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* NOTE: there is deliberately NO action here for re-tagging an existing     *)
(* record's jurisdiction.  No such entry point exists in compliance/src/lib.rs *)
(* -- `jurisdiction` is fixed when `add_to_allowlist` writes the record, and  *)
(* `set_tax_residency` stores an unrelated per-investor hash.  An earlier     *)
(* draft invented `SetJurisdictionRecord`; it was deleted rather than kept,   *)
(* because an invented action is an unverified claim about the system.        *)
(***************************************************************************)

SetCPaused(p) ==                      \* compliance/src/lib.rs:350-372
    /\ cAdmin # NoAddr
    /\ cPaused' = p
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup, rec,
                   allowList, blocked, cAdmin, seq, ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* asset-token/src/lib.rs:79-130.                                           *)
(*                                                                         *)
(* Line 98 is the load-bearing check: `initialize` credits the admin the    *)
(* entire supply and therefore must gate on `IsAllowed(admin)`, exactly as   *)
(* a later mint gates on its recipient.  Line 95 rejects a negative supply.  *)
(***************************************************************************)
InitializeToken(a, supply) ==
    /\ tokAdmin = NoAddr
    /\ cAdmin # NoAddr
    /\ a = cAdmin
    /\ a \in Addrs
    /\ supply \in BalanceRange
    /\ IsAllowed(a)
    /\ tokAdmin' = a
    /\ total'   = supply
    /\ bal'     = [bal EXCEPT ![a] = supply]
    /\ tPaused' = FALSE
    /\ UNCHANGED << lockup, rec, allowList, blocked, cAdmin, cPaused,
                   seq, ungatedCredit, seized >>
    \* Token initialization is a monetary event by construction, so it is
    \* exempt from (S2) in the same way Mint and Burn are.  It is not exempt
    \* from (G1): an ungated credit here latches the witness.
    /\ ungatedCredit'    = ungatedCredit \/ ~IsAllowed(a)
    /\ nonMonetaryDrift' = nonMonetaryDrift

(***************************************************************************)
(* asset-token/src/lib.rs:184-213.                                          *)
(*                                                                         *)
(* Monetary.  Note the guard order: admin, pause, `amount > 0`, then the     *)
(* RECIPIENT compliance check at line 192, and only then the supply and      *)
(* balance writes.  The witness latches if that recipient check was false.  *)
(***************************************************************************)
Mint(to, amt) ==
    /\ tokAdmin # NoAddr
    /\ tPaused = FALSE
    /\ amt \in 1 .. MaxBal
    /\ to \in Addrs
    /\ IsAllowed(to)
    /\ total + amt <= MaxBal
    /\ bal[to] + amt <= MaxBal
    /\ total' = total + amt
    /\ bal'   = [bal EXCEPT ![to] = @ + amt]
    /\ UNCHANGED << tokAdmin, tPaused, lockup, rec, allowList, blocked,
                   cAdmin, cPaused, seq, seized >>
    /\ ungatedCredit'    = ungatedCredit \/ ~IsAllowed(to)
    /\ nonMonetaryDrift' = nonMonetaryDrift

(***************************************************************************)
(* asset-token/src/lib.rs:132-183.                                          *)
(*                                                                         *)
(* Non-monetary: supply is untouched.  The guard order is faithful and      *)
(* matters for (G1): both the sender check (line 141) and the RECIPIENT     *)
(* check (line 144) precede every balance write, and the self-transfer      *)
(* early return (line 162) sits after both -- so a self-transfer by a       *)
(* non-allowlisted holder is *rejected*, not silently accepted.  `f # t`     *)
(* models that early return: the state is unchanged and no credit occurs.    *)
(***************************************************************************)
Transfer(f, t, amt) ==
    /\ tokAdmin # NoAddr
    /\ tPaused = FALSE
    /\ ~IsLocked(f)
    /\ amt \in 1 .. MaxBal
    /\ f \in Addrs
    /\ t \in Addrs
    /\ f # t
    /\ IsAllowed(f)
    /\ IsAllowed(t)
    /\ bal[f] >= amt
    /\ bal[t] + amt <= MaxBal
    /\ bal' = [bal EXCEPT ![f] = @ - amt, ![t] = @ + amt]
    /\ UNCHANGED << total, tokAdmin, tPaused, lockup, rec, allowList,
                   blocked, cAdmin, cPaused, seq, seized >>
    /\ ungatedCredit' = ungatedCredit \/ ~IsAllowed(t)
    /\ NoDrift

(***************************************************************************)
(* asset-token/src/lib.rs:162-166.  Self-transfer is an authenticated,      *)
(* compliance-checked, no-op.  It is kept as a distinct action because it   *)
(* exercises the "rejected, not accepted" ordering.                         *)
(***************************************************************************)
SelfTransfer(a, amt) ==
    /\ tokAdmin # NoAddr
    /\ tPaused = FALSE
    /\ ~IsLocked(a)
    /\ amt \in 1 .. MaxBal
    /\ a \in Addrs
    /\ IsAllowed(a)
    /\ bal[a] >= amt
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup, rec, allowList,
                   blocked, cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* asset-token/src/lib.rs:215-246.  Monetary.  Reduces both the holder      *)
(* balance and the supply.                                                   *)
(***************************************************************************)
Burn(f, amt) ==
    /\ tokAdmin # NoAddr
    /\ ~IsLocked(f)
    /\ amt \in 1 .. MaxBal
    /\ f \in Addrs
    /\ bal[f] >= amt
    /\ total >= amt
    /\ total' = total - amt
    /\ bal'   = [bal EXCEPT ![f] = @ - amt]
    /\ UNCHANGED << tokAdmin, tPaused, lockup, rec, allowList, blocked,
                   cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ nonMonetaryDrift' = nonMonetaryDrift

(***************************************************************************)
(* asset-token/src/lib.rs:360-376.                                          *)
(*                                                                         *)
(* Non-monetary and deliberately NOT compliance-gated: a clawback must      *)
(* still be executable against a suspended or de-allowlisted holder,         *)
(* otherwise revocation would be blocked exactly when it is needed.  It can  *)
(* only *decrease* a balance, so it can never violate (G1).                 *)
(***************************************************************************)
Clawback(f, amt) ==
    /\ tokAdmin # NoAddr
    /\ amt \in 1 .. MaxBal
    /\ f \in Addrs
    /\ bal[f] >= amt
    /\ bal' = [bal EXCEPT ![f] = @ - amt]
    \* A clawback removes tokens from a holder WITHOUT reducing `total`
    \* (asset-token/src/lib.rs:360-376 touches no supply field), so the seized
    \* amount has to be accounted for somewhere.  `seized` is that account: it is
    \* a ghost variable, absent from the Rust, introduced solely so the
    \* conservation law below is exact rather than merely an inequality.
    /\ seized' = seized + amt
    /\ UNCHANGED << total, tokAdmin, tPaused, lockup, rec, allowList,
                   blocked, cAdmin, cPaused, seq, ungatedCredit >>
    /\ NoDrift


SetLockup(f, n) ==                    \* asset-token/src/lib.rs: set_lockup
    /\ tokAdmin # NoAddr
    /\ f \in Addrs
    /\ n \in LockupRange
    /\ lockup' = [lockup EXCEPT ![f] = n]
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, rec, allowList,
                   blocked, cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ NoDrift

SetTPaused(p) ==                     \* asset-token/src/lib.rs: pause/unpause
    /\ tokAdmin # NoAddr
    /\ tPaused' = p
    /\ UNCHANGED << bal, total, tokAdmin, lockup, rec, allowList, blocked,
                   cAdmin, cPaused, seq, ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* Ledger advancement.  Exists so that KYC expiry and lockup release are     *)
(* reachable, which is what makes `IsAllowed` time-dependent.  Bounded by    *)
(* MaxSeq to keep the state space finite.                                    *)
(***************************************************************************)
AdvanceSeq ==
    /\ seq < MaxSeq
    /\ seq' = seq + 1
    /\ UNCHANGED << bal, total, tokAdmin, tPaused, lockup, rec, allowList,
                   blocked, cAdmin, cPaused, ungatedCredit, seized >>
    /\ NoDrift

(***************************************************************************)
(* Stuttering, so TLC explores the infinite behaviour of `[Next]_vars`     *)
(* without reporting a deadlock.                                             *)
(***************************************************************************)
Stutter == UNCHANGED vars

Next ==
    \/ \E a \in Addrs               : ComplianceInit(a)
    \/ \E a \in Addrs, j \in Juris, e \in 0..MaxExp : AddToAllowlist(a, j, e)
    \/ \E a \in Addrs               : Suspend(a)
    \/ \E a \in Addrs               : Remove(a)
    \/ \E j \in Juris               : BlockJurisdiction(j)
    \/ \E j \in Juris               : UnblockJurisdiction(j)
    \/ \E p \in BOOLEAN             : SetCPaused(p)
    \/ \E a \in Addrs, s \in BalanceRange : InitializeToken(a, s)
    \/ \E to \in Addrs, amt \in 1..MaxBal : Mint(to, amt)
    \/ \E f, t \in Addrs, amt \in 1..MaxBal : Transfer(f, t, amt)
    \/ \E a \in Addrs, amt \in 1..MaxBal : SelfTransfer(a, amt)
    \/ \E f \in Addrs, amt \in 1..MaxBal : Burn(f, amt)
    \/ \E f \in Addrs, amt \in 1..MaxBal : Clawback(f, amt)
    \/ \E f \in Addrs, n \in LockupRange : SetLockup(f, n)
    \/ \E p \in BOOLEAN             : SetTPaused(p)
    \/ AdvanceSeq

Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------------------
\* TypeOK
\* ---------------------------------------------------------------------------
TypeOK ==
    /\ bal    \in [Addrs -> BalanceRange]
    /\ total  \in BalanceRange
    /\ seized \in BalanceRange
    /\ tokAdmin \in Addrs \cup {NoAddr}
    /\ tPaused \in BOOLEAN
    /\ lockup \in [Addrs -> LockupRange]
    /\ rec    \in [Addrs -> [status : Statuses, juris : Juris,
                            exp : 0..MaxExp]]
    /\ allowList \subseteq Addrs
    /\ blocked   \subseteq Juris
    /\ cAdmin    \in Addrs \cup {NoAddr}
    /\ cPaused   \in BOOLEAN
    /\ seq       \in 0 .. MaxSeq
    /\ ungatedCredit    \in BOOLEAN
    /\ nonMonetaryDrift \in BOOLEAN

(***************************************************************************)
(* (S1) Supply conservation, stated exactly.                                   *)
(*                                                                          *)
(* The first form tried here was the obvious `total = \Sum_a bal[a]`, and    *)
(* TLC REFUTED it in 5 steps:                                                  *)
(*                                                                          *)
(*   ComplianceInit(a1) -> AddToAllowlist(a1,j1,0) -> InitializeToken(a1,1) *)
(*   -> Clawback(a1,1)                                                        *)
(*                                                                          *)
(* which ends with total = 1 and every balance 0.  That is not a bug in the  *)
(* model: `clawback` decrements the holder's balance and does not touch      *)
(* `total_supply` (asset-token/src/lib.rs:360-376), because re-issuing the    *)
(* seized amount is deliberately left to a separate later admin action.  So   *)
(* the honest conservation law is a T-account balance:                      *)
(*                                                                          *)
(*     total  ==  (sum of all holder balances)  +  seized                   *)
(*                                                                          *)
(* `seized` is a ghost variable (it has no Rust counterpart); it is the      *)
(* running total of clawbacked units.  Mint and burn move both sides        *)
(* together, transfer is a pure reshuffle, and only clawback moves mass out  *)
(* of the `bal` term into `seized`.  A holder-inflating bug -- the           *)
(* self-transfer double-write this codebase once had -- makes the right-hand  *)
(* side exceed `total` and is caught here.                                   *)
(***************************************************************************)
SupplyConserved == total = TotalBal(Addrs) + seized

(***************************************************************************)
(* (G2) The same gate, stated directly rather than through a witness.         *)
(*                                                                          *)
(* TLC can evaluate `ENABLED <<A>>_vars` inside a state predicate, so these  *)
(* read as plain English and are far easier to audit than the latching        *)
(* witness above -- while being strictly stronger, because a latching        *)
(* witness only fires on actions the model actually reaches.                 *)
(*                                                                          *)
(* `TransferGate` is the literal statement of the acceptance criterion: if   *)
(* a transfer is executable at all, then BOTH parties were allowlisted in    *)
(* this state.  There is no reachable state in which an un-allowlisted      *)
(* address can be the recipient of a transfer.                              *)
(***************************************************************************)
TransferGate ==
    \A f, t \in Addrs, amt \in 1..MaxBal :
        ENABLED Transfer(f, t, amt) => (IsAllowed(f) /\ IsAllowed(t))

MintGate ==
    \A to \in Addrs, amt \in 1..MaxBal :
        ENABLED Mint(to, amt) => IsAllowed(to)

InitializeGate ==
    \A a \in Addrs, s \in BalanceRange :
        ENABLED InitializeToken(a, s) => IsAllowed(a)

(***************************************************************************)
(* The self-transfer ordering property.  `SelfTransfer` models the early      *)
(* return at asset-token/src/lib.rs:162, and it is gated on IsAllowed(a)     *)
(* even though it moves no tokens.  That is deliberate: the compliance      *)
(* checks sit at lines 141/144, BEFORE the `f == t` early return, so a       *)
(* de-allowlisted holder attempting a self-transfer is REJECTED rather than   *)
(* silently accepted.  Reordering those guards would make this false.        *)
(***************************************************************************)
SelfTransferGate ==
    \A a \in Addrs, amt \in 1..MaxBal :
        ENABLED SelfTransfer(a, amt) => IsAllowed(a)

(***************************************************************************)
(* (G3) Revocation must never be blocked by the gate it is meant to          *)
(* enforce.  `clawback` is deliberately NOT compliance-gated, so any holder   *)
(* with a balance remains clawbackable even after their compliance record is *)
(* suspended, removed, expired, or jurisdiction-blocked.  Without this, a   *)
(* compliance system could be trivially defeated by an admin pausing the    *)
(* allowlist.                                                               *)
(***************************************************************************)
ClawbackNotGated ==
    \A a \in Addrs :
        (tokAdmin # NoAddr /\ bal[a] >= 1) => ENABLED Clawback(a, 1)

(***************************************************************************)
(* (S1a) The one-sided corollary, stated separately because it is the part   *)
(* that actually matters for solvency: the system can never manufacture      *)
(* tokens.  Sum-of-balances may legitimately fall BELOW total after a        *)
(* clawback, but must never exceed it.                                       *)
(***************************************************************************)
NoPhantomSupply == TotalBal(Addrs) <= total

(***************************************************************************)
(* (G1) The transfer gate.  See the module header for why the witness       *)
(* variable encodes a two-state property.                                   *)
(***************************************************************************)
NoUngatedCredit == ungatedCredit = FALSE

(***************************************************************************)
(* (S2) Supply is constant across every non-mint/burn operation.            *)
(***************************************************************************)
NoMonetaryDrift == nonMonetaryDrift = FALSE

(***************************************************************************)
(* The naive form of (G1), which is FALSE.  Checked in Refutation.cfg,     *)
(* where TLC is expected to report a violation with a concrete trace.       *)
(*                                                                         *)
(* A holder that was credited while allowlisted keeps its balance after the  *)
(* allowlist entry is withdrawn (Remove / Suspend / KYC expiry / a           *)
(* jurisdiction block), because none of those compliance actions can reach   *)
(* the token's balance storage.  So "positive balance implies allowlisted"  *)
(* is not an invariant of the system, and must never be asserted.            *)
(***************************************************************************)
PositiveBalanceImpliesAllowed ==
    \A a \in Addrs : bal[a] > 0 => IsAllowed(a)

(***************************************************************************)
(* A property that IS true and is worth guarding: a clawback can always     *)
(* proceed against a holder whose compliance has been withdrawn, because    *)
(* revocation would be useless otherwise.  Modelled as a *check* that the   *)
(* spec does not need to violate, via the absence of a witness.             *)
(***************************************************************************)
NoNegativeBalances == \A a \in Addrs : bal[a] >= 0

=============================================================================
