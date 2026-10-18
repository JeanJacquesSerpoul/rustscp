# Serveur FTP de test (vsftpd) : FTP en clair et FTPS explicite (certificat auto-signé),
# utilisateur « test », mot de passe « test »
FROM debian:trixie-slim
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        vsftpd openssl && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/bash test && echo 'test:test' | chpasswd \
    && mkdir -p /var/run/vsftpd/empty \
    && openssl req -x509 -nodes -newkey rsa:2048 -days 3650 -subj "/CN=localhost" \
        -keyout /etc/ssl/private/vsftpd.key -out /etc/ssl/certs/vsftpd.pem
COPY vsftpd.conf /etc/vsftpd.conf
EXPOSE 21 30000-30009
CMD ["/usr/sbin/vsftpd", "/etc/vsftpd.conf"]
