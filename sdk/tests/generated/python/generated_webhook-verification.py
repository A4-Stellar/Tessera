import pytest
from tessera_sdk import TesseraClient

API_BASE = "http://localhost:8080"
client = TesseraClient(API_BASE)

class TestSDKPythonExamples:
  async def test_python_example_1_webhook-verification(self):
    # Source: docs/content/en/docs/webhook-verification.mdx:77
    import hashlib
    import hmac
    import re
    
    
    def verify_webhook(
        secret: str, payload: bytes, signature_headers: list[str]
    ) -> bool:
        expected = hmac.new(
            secret.encode(), payload, hashlib.sha256
        ).hexdigest()
    
        for header in signature_headers:
            match = re.search(r"sha256=([a-f0-9]{64})", header)
            if match and hmac.compare_digest(match.group(1), expected):
                return True
        return False
    
    
    # Flask example
    @app.route("/webhook", methods=["POST"])
    def webhook():
        payload = request.get_data()
        signatures = request.headers.getlist("X-Tessera-Signature")
    
        if not verify_webhook(WEBHOOK_SECRET, payload, signatures):
            return "Invalid signature", 401
        # Process event...
        return "OK", 200

