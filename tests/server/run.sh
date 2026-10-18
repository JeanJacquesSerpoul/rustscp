#!/bin/sh
# Lance le serveur de test sur localhost:2222 (utilisateur "test", clé tests/server/test_key, secret TOTP JBSWY3DPEHPK3PXP)
cd "$(dirname "$0")"
docker build -q -t rustscp-test-sshd . && docker rm -f rustscp-test-sshd >/dev/null 2>&1
exec docker run -d --name rustscp-test-sshd -p 127.0.0.1:2222:22 rustscp-test-sshd
