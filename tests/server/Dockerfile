# Serveur SSH de test : clé publique + code TOTP (Google Authenticator) obligatoires
FROM debian:trixie-slim
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        openssh-server openssh-client libpam-google-authenticator && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /run/sshd && useradd -m -s /bin/bash test
COPY test_key.pub /home/test/.ssh/authorized_keys
RUN printf 'JBSWY3DPEHPK3PXP\n" RATE_LIMIT 10 30\n" WINDOW_SIZE 3\n" DISALLOW_REUSE\n" TOTP_AUTH\n' > /home/test/.google_authenticator \
    && chown -R test:test /home/test && chmod 700 /home/test/.ssh && chmod 600 /home/test/.ssh/authorized_keys \
    && chmod 400 /home/test/.google_authenticator \
    && sed -i 's/^@include common-auth/#@include common-auth/' /etc/pam.d/sshd \
    && echo 'auth required pam_google_authenticator.so' >> /etc/pam.d/sshd \
    && printf 'UsePAM yes\nKbdInteractiveAuthentication yes\nPasswordAuthentication no\nPubkeyAuthentication yes\nAuthenticationMethods publickey,keyboard-interactive\n' > /etc/ssh/sshd_config.d/wscp.conf
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
