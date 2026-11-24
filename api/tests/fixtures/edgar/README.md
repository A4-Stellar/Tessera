# EDGAR golden fixtures (synthetic)

Fixtures for the Form 4 import and cap-table reconciliation (issue #476,
`api/src/services/edgar.rs`, `docs/lib/cap-table-reconcile.ts`).

These files are **synthetic**. They follow the structure of real SEC EDGAR
data, so the parser is exercised on the same shapes it reads in production:

- `submissions-CIK0009990001.json` uses the `https://data.sec.gov/submissions/CIK##########.json`
  layout (`filings.recent` column arrays).
- `*.xml` are Form 4 `ownershipDocument`s (schema X0508) as served from
  `https://www.sec.gov/Archives/edgar/data/<cik>/<accession>/form4.xml`.

The issuer (CIK 0009990001, "Atlas Residences Trust"), the reporting owners
and every amount are fictional. They are not real filings and must not be
cited as such. The 999xxxx CIK range was chosen to stay clear of real
registrants used in tests.

What each filing covers:

| accession | owner | case |
|---|---|---|
| 0009990101-26-000011 | DOE JANE (0009990101) | first purchase: 120 000 direct |
| 0009990101-26-000027 | DOE JANE | buy 5 000 then sell 10 000: 115 000 direct, plus a 20 000 indirect holding |
| 0009990102-26-000004 | ROE RICHARD (0009990102) | 40 000 direct |
| 0009990103-26-000009 | SMITH JOHN (0009990103) and SMITH & CO HOLDINGS LLC (0009990104) | joint filing, one 60 000 position |

`docs/__tests__/fixtures/edgar-form4-holders.csv` is the same data exported
as the CSV the docs component accepts.
