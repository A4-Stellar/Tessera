requires "mint_burn.k"

module MINT-BURN-SPEC
  imports MINT-BURN
  imports MAP-SYMBOLIC

  claim [initialize-conservation]:
    <k> initialize(N, true) ~> audit => true </k>
    <admin> ADMIN </admin> <initialized> false => true </initialized>
    <balances> .Map => ADMIN |-> N </balances>
    <supply> 0 => N </supply> <result> _ => true </result>
    requires 0 <=Int N andBool N <=Int maxSupply

  claim [mint-existing-conservation]:
    <k> mint(ADMIN, TO, N, true, true) ~> audit => true </k>
    <admin> ADMIN </admin> <initialized> true </initialized>
    <paused> false </paused> <liquidated> false </liquidated>
    <balances> (TO |-> B:Int => TO |-> (B +Int N)) REST:Map </balances>
    <supply> S => S +Int N </supply> <result> _ => true </result>
    requires S ==Int B +Int sumBalances(REST) andBool N >Int 0
      andBool B >=Int 0 andBool S >=Int 0
      andBool S +Int N <=Int maxSupply andBool B +Int N <=Int maxSupply

  claim [mint-new-conservation]:
    <k> mint(ADMIN, TO, N, true, true) ~> audit => true </k>
    <admin> ADMIN </admin> <initialized> true </initialized>
    <paused> false </paused> <liquidated> false </liquidated>
    <balances> M => (TO |-> N) M </balances>
    <supply> S => S +Int N </supply> <result> _ => true </result>
    requires S ==Int sumBalances(M) andBool notBool (TO in_keys(M))
      andBool N >Int 0 andBool S >=Int 0 andBool S +Int N <=Int maxSupply

  claim [burn-conservation]:
    <k> burn(FROM, N, true, false) ~> audit => true </k>
    <initialized> true </initialized> <liquidated> false </liquidated>
    <balances> (FROM |-> B:Int => FROM |-> (B -Int N)) REST:Map </balances>
    <supply> S => S -Int N </supply> <result> _ => true </result>
    requires S ==Int B +Int sumBalances(REST) andBool N >Int 0
      andBool B >=Int N andBool S >=Int N andBool S <=Int maxSupply

  // Arbitrary amounts and arbitrary maps: no authorized identity/signature,
  // no creation. This is not restricted to finitely enumerated test states.
  claim [unauthorized-mint-is-identity]:
    <k> mint(CALLER, _TO, _N, AUTH, _COMPLIANT) => .K </k>
    <admin> ADMIN </admin> <balances> _M </balances> <supply> _S </supply>
    <result> _ => false </result>
    requires CALLER =/=Int ADMIN orBool notBool AUTH

  claim [mint-overflow-is-identity]:
    <k> mint(_CALLER, _TO, N, _AUTH, _COMPLIANT) => .K </k>
    <balances> _M </balances> <supply> S </supply> <result> _ => false </result>
    requires S +Int N >Int maxSupply

  claim [nonpositive-mint-is-identity]:
    <k> mint(_CALLER, _TO, N, _AUTH, _COMPLIANT) => .K </k>
    <balances> _M </balances> <supply> _S </supply> <result> _ => false </result>
    requires N <=Int 0

  claim [unauthorized-burn-is-identity]:
    <k> burn(_FROM, _N, false, _LOCKED) => .K </k>
    <balances> _M </balances> <supply> _S </supply> <result> _ => false </result>

  claim [insufficient-balance-burn-is-identity]:
    <k> burn(FROM, N, true, false) => .K </k>
    <initialized> true </initialized> <liquidated> false </liquidated>
    <balances> (FROM |-> B:Int) _REST:Map </balances>
    <supply> _S </supply> <result> _ => false </result>
    requires N >Int 0 andBool B >=Int 0 andBool B <Int N

  claim [missing-account-burn-is-identity]:
    <k> burn(FROM, N, true, false) => .K </k>
    <initialized> true </initialized> <liquidated> false </liquidated>
    <balances> M </balances> <supply> _S </supply> <result> _ => false </result>
    requires N >Int 0 andBool notBool (FROM in_keys(M))

  claim [paused-mint-is-identity]:
    <k> mint(_CALLER, _TO, _N, _AUTH, _COMPLIANT) => .K </k>
    <paused> true </paused>
    <balances> _M </balances> <supply> _S </supply> <result> _ => false </result>

  claim [liquidated-mint-is-identity]:
    <k> mint(_CALLER, _TO, _N, _AUTH, _COMPLIANT) => .K </k>
    <liquidated> true </liquidated>
    <balances> _M </balances> <supply> _S </supply> <result> _ => false </result>

  claim [locked-burn-is-identity]:
    <k> burn(_FROM, _N, _AUTH, true) => .K </k>
    <balances> _M </balances> <supply> _S </supply> <result> _ => false </result>

  claim [nonpositive-burn-is-identity]:
    <k> burn(_FROM, N, _AUTH, _LOCKED) => .K </k>
    <balances> _M </balances> <supply> _S </supply> <result> _ => false </result>
    requires N <=Int 0

  claim [reinitialization-is-identity]:
    <k> initialize(_N, _AUTH) => .K </k>
    <initialized> true </initialized>
    <balances> _M </balances> <supply> _S </supply> <result> _ => false </result>
endmodule
