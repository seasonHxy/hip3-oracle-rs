"""Offline oracle signature fixtures using pinned official SDK; never contacts a chain."""
import json
from pathlib import Path
from eth_account import Account
from hyperliquid.utils.signing import action_hash, sign_l1_action
from hyperliquid.exchange import Exchange
from unittest.mock import Mock, patch

wallet = Account.from_key(bytes([1]) * 32)  # public test fixture, NEVER fund
vectors = []
for mainnet in (False, True):
    for marks in ([], [{"demo:TSLA": "390.22", "demo:AAPL": "230.2"}]):
        e = Exchange.__new__(Exchange)
        e.wallet = wallet
        e.base_url = "https://api.hyperliquid.xyz" if mainnet else "https://api.hyperliquid-testnet.xyz"
        e.expires_after = None
        e._post_action = Mock()
        with patch("hyperliquid.exchange.get_timestamp_ms", return_value=1700000000123):
            e.perp_deploy_set_oracle("demo", {"demo:TSLA": "390.22", "demo:AAPL": "230.2"}, marks, {"demo:TSLA": "390.22", "demo:AAPL": "230.2"})
        action, signature, nonce = e._post_action.call_args.args
        assert signature == sign_l1_action(wallet, action, None, nonce, None, mainnet)
        vectors.append(dict(mainnet=mainnet, action=action, nonce=nonce, hash=action_hash(action, None, nonce, None).hex(), signature=signature))
Path(__file__).with_name("sdk_vectors.json").write_text(json.dumps(vectors, indent=2)+"\n")
