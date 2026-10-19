import os
import schemathesis

BASE_URL = os.getenv("TESSERA_API_URL", "http://127.0.0.1:8080")
SCHEMA = os.getenv("TESSERA_OPENAPI_URL", f"{BASE_URL}/openapi.json")
schema = schemathesis.from_uri(SCHEMA)


@schema.parametrize()
def test_api_rejects_malformed_requests(case):
    response = case.call(base_url=BASE_URL)
    assert response.status_code < 500, response.text[:500]
