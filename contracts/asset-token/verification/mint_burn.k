// Transaction-level semantics for the token's integer accounting. Authorization
// flags stand for successful Soroban require_auth and compliance calls. Rejected
// invocations are atomic identities, as in the Soroban host's rollback semantics.
requires "domains.md"

module MINT-BURN-SYNTAX
  imports INT-SYNTAX
  imports BOOL-SYNTAX
  syntax Command ::= initialize(Int, Bool)
                   | mint(Int, Int, Int, Bool, Bool)
                   | burn(Int, Int, Bool, Bool)
                   | "audit"
  syntax KItem ::= Command
endmodule

module MINT-BURN
  imports MINT-BURN-SYNTAX
  imports INT
  imports BOOL
  imports MAP
  imports K-EQUAL

  configuration <token>
    <k> $PGM:K </k>
    <admin> 0 </admin>
    <initialized> false </initialized>
    <paused> false </paused>
    <liquidated> false </liquidated>
    <supply> 0 </supply>
    <balances> .Map </balances>
    <result> false </result>
  </token>

  // SMT sees the untouched frame's sum as an uninterpreted integer;
  // K unfolds the actual modified map entry using the recursive rule below.
  syntax Int ::= "maxSupply" [function, total]
               | sumBalances(Map) [function, smtlib(sumBalances)]
  rule maxSupply => 170141183460469231731687303715884105727
  rule sumBalances(.Map) => 0
  rule sumBalances((_:Int |-> B:Int) REST:Map) => B +Int sumBalances(REST)

  rule <k> initialize(N, true) => .K ... </k>
       <admin> ADMIN </admin>
       <initialized> false => true </initialized>
       <balances> .Map => ADMIN |-> N </balances>
       <supply> 0 => N </supply>
       <result> _ => true </result>
    requires 0 <=Int N andBool N <=Int maxSupply

  rule <k> initialize(_, _) => .K ... </k>
       <initialized> true </initialized>
       <result> _ => false </result>
  rule <k> initialize(N, AUTH) => .K ... </k>
       <initialized> false </initialized>
       <result> _ => false </result>
    requires notBool AUTH orBool N <Int 0 orBool N >Int maxSupply

  // Existing and new accounts are separate rules: a map binding is unique;
  // missing accounts have balance zero and are materialized only on success.
  rule <k> mint(ADMIN, TO, N, true, true) => .K ... </k>
       <admin> ADMIN </admin> <initialized> true </initialized>
       <paused> false </paused> <liquidated> false </liquidated>
       <balances> (TO |-> B:Int => TO |-> (B +Int N)) _REST:Map </balances>
       <supply> S => S +Int N </supply> <result> _ => true </result>
    requires N >Int 0 andBool B >=Int 0 andBool S >=Int 0
     andBool S +Int N <=Int maxSupply andBool B +Int N <=Int maxSupply

  rule <k> mint(ADMIN, TO, N, true, true) => .K ... </k>
       <admin> ADMIN </admin> <initialized> true </initialized>
       <paused> false </paused> <liquidated> false </liquidated>
       <balances> M => (TO |-> N) M </balances>
       <supply> S => S +Int N </supply> <result> _ => true </result>
    requires notBool (TO in_keys(M)) andBool N >Int 0 andBool S >=Int 0
     andBool S +Int N <=Int maxSupply

  rule <k> mint(CALLER, _, N, AUTH, COMPLIANT) => .K ... </k>
       <admin> ADMIN </admin> <initialized> INIT </initialized>
       <paused> PAUSED </paused> <liquidated> LIQUIDATED </liquidated>
       <supply> S </supply> <result> _ => false </result>
    requires CALLER =/=Int ADMIN orBool notBool AUTH orBool notBool COMPLIANT
      orBool notBool INIT orBool PAUSED orBool LIQUIDATED
      orBool N <=Int 0 orBool S +Int N >Int maxSupply

  // Burn is holder-authorized, not admin-authorized. LOCKED abstracts the
  // ledger lockup guard. Ordinary burns are disabled after liquidation;
  // vault burns have the same accounting with additional vault authorization.
  rule <k> burn(FROM, N, true, false) => .K ... </k>
       <initialized> true </initialized> <liquidated> false </liquidated>
       <balances> (FROM |-> B:Int => FROM |-> (B -Int N)) _REST:Map </balances>
       <supply> S => S -Int N </supply> <result> _ => true </result>
    requires N >Int 0 andBool B >=Int N andBool S >=Int N
     andBool S <=Int maxSupply

  rule <k> burn(_, N, AUTH, LOCKED) => .K ... </k>
       <initialized> INIT </initialized> <liquidated> LIQUIDATED </liquidated>
       <result> _ => false </result>
    requires notBool INIT orBool LIQUIDATED orBool notBool AUTH
      orBool LOCKED orBool N <=Int 0
  rule <k> burn(FROM, N, true, false) => .K ... </k>
       <initialized> true </initialized> <liquidated> false </liquidated>
       <balances> (FROM |-> B:Int) _:Map </balances>
       <result> _ => false </result>
    requires N >Int 0 andBool B <Int N
  rule <k> burn(FROM, N, true, false) => .K ... </k>
       <initialized> true </initialized> <liquidated> false </liquidated>
       <balances> M </balances> <result> _ => false </result>
    requires N >Int 0 andBool notBool (FROM in_keys(M))

  rule <k> audit => (S ==Int sumBalances(M)) ... </k>
       <balances> M </balances> <supply> S </supply>
endmodule
