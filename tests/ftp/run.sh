#!/bin/sh
# Lance le serveur FTP de test sur localhost:2121 (utilisateur « test », mot de passe « test »,
# ports passifs 30000-30009 ; FTPS explicite avec certificat auto-signé)
cd "$(dirname "$0")"
docker build -q -t rustscp-test-ftpd . && docker rm -f rustscp-test-ftpd >/dev/null 2>&1
exec docker run -d --init --name rustscp-test-ftpd -p 127.0.0.1:2121:21 -p 127.0.0.1:30000-30009:30000-30009 rustscp-test-ftpd
