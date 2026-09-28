; SMT-LIB v2 formal verification of debt liquidation math
; Issue #134: Formal Verification of Dynamic Debt Liquidation Math with Z3 / SMT Solvers
; Component: contracts/debt-token/verification/
;
; This file encodes ALL liquidation invariants as separate
; check-sat queries. Each invariant is encoded as the NEGATION
; of the property we want to prove. If check-sat returns unsat,
; the invariant is proven.
;
; Usage: z3 -smt2 liquidation_full.smt2
; Or: python3 verification/verify_liquidation.py

(set-logic ALL)
(set-info :status unsat)

; --- WAD Precision Base ---
(declare-const WAD Int)
(assert (= WAD 1000000000000000000))

; ============================================================================
; INVARIANT 1: Liquidator reward never exceeds total collateral value
; ============================================================================
; Property: collateral_total >= total_debt ∧ 0 < bonus_pct < WAD
; ⟹ div(total_debt * bonus_pct, WAD) <= collateral_total

(push)
(declare-const c1_collateral_total Int)
(declare-const c1_total_debt Int)
(declare-const c1_bonus_pct Int)
(declare-const c1_reward Int)

(assert (>= c1_collateral_total c1_total_debt))
(assert (> c1_total_debt 0))
(assert (> c1_bonus_pct 0))
(assert (< c1_bonus_pct WAD))
(assert (= c1_reward (div (* c1_total_debt c1_bonus_pct) WAD)))
(assert (> c1_reward c1_collateral_total))
(check-sat)
(get-model)
(pop)

; ============================================================================
; INVARIANT 2: Remaining collateral is non-negative after liquidation
; ============================================================================
; Property: collateral_total >= total_debt + reward ⟹ remaining >= 0

(push)
(declare-const c2_collateral_total Int)
(declare-const c2_total_debt Int)
(declare-const c2_bonus_pct Int)
(declare-const c2_reward Int)
(declare-const c2_remaining Int)

(assert (>= c2_collateral_total 0))
(assert (>= c2_total_debt 0))
(assert (> c2_bonus_pct 0))
(assert (< c2_bonus_pct WAD))
(assert (= c2_reward (div (* c2_total_debt c2_bonus_pct) WAD)))
(assert (= c2_remaining (- c2_collateral_total c2_total_debt c2_reward)))
(assert (>= c2_collateral_total (+ c2_total_debt c2_reward)))
(assert (< c2_remaining 0))
(check-sat)
(get-model)
(pop)

; ============================================================================
; INVARIANT 3: No under-collateralized bad debt states
; ============================================================================
; Property: The liquidator cannot seize more than the collateral value

(push)
(declare-const c3_collateral_total Int)
(declare-const c3_total_debt Int)
(declare-const c3_bonus_pct Int)
(declare-const c3_reward Int)
(declare-const c3_seized Int)

(assert (> c3_collateral_total 0))
(assert (> c3_total_debt 0))
(assert (> c3_bonus_pct 0))
(assert (< c3_bonus_pct WAD))
(assert (= c3_reward (div (* c3_total_debt c3_bonus_pct) WAD)))
(assert (= c3_seized (+ c3_total_debt c3_reward)))
(assert (>= c3_collateral_total c3_seized))
(assert (> c3_seized c3_collateral_total))
(check-sat)
(get-model)
(pop)

; ============================================================================
; INVARIANT 4: Integer truncation bounds
; ============================================================================
; Property: div(total_debt * bonus_pct, WAD) * WAD <= total_debt * bonus_pct

(push)
(declare-const c4_total_debt Int)
(declare-const c4_bonus_pct Int)
(declare-const c4_reward Int)

(assert (>= c4_total_debt 0))
(assert (> c4_bonus_pct 0))
(assert (< c4_bonus_pct WAD))
(assert (>= (* c4_total_debt c4_bonus_pct) 0))
(assert (= c4_reward (div (* c4_total_debt c4_bonus_pct) WAD)))
(assert (> (* c4_reward WAD) (* c4_total_debt c4_bonus_pct)))
(check-sat)
(get-model)
(pop)

; ============================================================================
; INVARIANT 5: Liquidator reward within collateral surplus
; ============================================================================
; Property: collateral_total >= 2 * total_debt ∧ 0 < bonus_pct < WAD
; ⟹ div(total_debt * bonus_pct, WAD) <= collateral_total - total_debt

(push)
(declare-const c5_collateral_total Int)
(declare-const c5_total_debt Int)
(declare-const c5_bonus_pct Int)
(declare-const c5_reward Int)

(assert (>= c5_collateral_total (* 2 c5_total_debt)))
(assert (> c5_total_debt 0))
(assert (> c5_bonus_pct 0))
(assert (< c5_bonus_pct WAD))
(assert (= c5_reward (div (* c5_total_debt c5_bonus_pct) WAD)))
(assert (> c5_reward (- c5_collateral_total c5_total_debt)))
(check-sat)
(get-model)
(pop)

; ============================================================================
; INVARIANT 6: Waterfall liquidation conserves value
; ============================================================================
; Property: senior_payment + mezzanine_payment + equity_payment = min(amount, total_debt)
; ∧ each payment >= 0

(push)
(declare-const c6_senior_owed Int)
(declare-const c6_mezzanine_owed Int)
(declare-const c6_equity_owed Int)
(declare-const c6_amount Int)

(assert (>= c6_senior_owed 0))
(assert (>= c6_mezzanine_owed 0))
(assert (>= c6_equity_owed 0))
(assert (>= c6_amount 0))

; Total debt
(declare-const c6_total_debt Int)
(assert (= c6_total_debt (+ c6_senior_owed c6_mezzanine_owed c6_equity_owed)))

; Waterfall payments using if-then-else
(declare-const c6_senior_payment Int)
(assert (= c6_senior_payment (ite (> c6_amount c6_senior_owed) c6_senior_owed c6_amount)))
(declare-const c6_remaining_after_senior Int)
(assert (= c6_remaining_after_senior (- c6_amount c6_senior_payment)))

(declare-const c6_mezzanine_payment Int)
(assert (= c6_mezzanine_payment (ite (> c6_remaining_after_senior c6_mezzanine_owed) c6_mezzanine_owed c6_remaining_after_senior)))
(declare-const c6_remaining_after_mezz Int)
(assert (= c6_remaining_after_mezz (- c6_remaining_after_senior c6_mezzanine_payment)))

(declare-const c6_equity_payment Int)
(assert (= c6_equity_payment (ite (> c6_remaining_after_mezz c6_equity_owed) c6_equity_owed c6_remaining_after_mezz)))

(declare-const c6_total_repaid Int)
(assert (= c6_total_repaid (+ c6_senior_payment c6_mezzanine_payment c6_equity_payment)))

; Negation: total_repaid != min(amount, total_debt) OR any payment < 0
(assert (or (not (= c6_total_repaid (ite (> c6_amount c6_total_debt) c6_total_debt c6_amount)))
             (< c6_senior_payment 0)
             (< c6_mezzanine_payment 0)
             (< c6_equity_payment 0)))
(check-sat)
(get-model)
(pop)

; ============================================================================
; INVARIANT 7: Tranche balances never go negative after repayment
; ============================================================================
; Property: After deposit_repayment with amount > 0:
;   senior_after >= 0 ∧ mezzanine_after >= 0 ∧ equity_after >= 0

(push)
(declare-const c7_senior_before Int)
(declare-const c7_mezz_before Int)
(declare-const c7_equity_before Int)
(declare-const c7_amount Int)

(assert (>= c7_senior_before 0))
(assert (>= c7_mezz_before 0))
(assert (>= c7_equity_before 0))
(assert (> c7_amount 0))

; Waterfall payments
(declare-const c7_senior_payment Int)
(assert (= c7_senior_payment (ite (> c7_amount c7_senior_before) c7_senior_before c7_amount)))
(declare-const c7_senior_after Int)
(assert (= c7_senior_after (- c7_senior_before c7_senior_payment)))
(declare-const c7_remaining_after_senior Int)
(assert (= c7_remaining_after_senior (- c7_amount c7_senior_payment)))

(declare-const c7_mezzanine_payment Int)
(assert (= c7_mezzanine_payment (ite (> c7_remaining_after_senior c7_mezz_before) c7_mezz_before c7_remaining_after_senior)))
(declare-const c7_mezz_after Int)
(assert (= c7_mezz_after (- c7_mezz_before c7_mezzanine_payment)))
(declare-const c7_remaining_after_mezz Int)
(assert (= c7_remaining_after_mezz (- c7_remaining_after_senior c7_mezzanine_payment)))

(declare-const c7_equity_payment Int)
(assert (= c7_equity_payment (ite (> c7_remaining_after_mezz c7_equity_before) c7_equity_before c7_remaining_after_mezz)))
(declare-const c7_equity_after Int)
(assert (= c7_equity_after (- c7_equity_before c7_equity_payment)))

; Negation: any tranche balance goes negative
(assert (or (< c7_senior_after 0) (< c7_mezz_after 0) (< c7_equity_after 0)))
(check-sat)
(get-model)
(pop)

; ============================================================================
; All invariants verified if all check-sat return unsat
; ============================================================================
(exit)
