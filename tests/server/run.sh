#!/bin/sh
# Lance le serveur de test sur localhost:2222 (utilisateur "test", clé tests/server/test_key, secret TOTP JBSWY3DPEHPK3PXP)
cd "$(dirname "$0")"
docker build -q -t wscp-test-sshd . && docker rm -f wscp-test-sshd >/dev/null 2>&1
exec docker run -d --name wscp-test-sshd -p 127.0.0.1:2222:22 wscp-test-sshd
