#!/bin/sh
#@tags: usage:vps, scope:apps, os:debian, os:ubuntu
# Apps: Certbot Configuration for VPS

set -eu

if [ -z "${VPS_DOMAIN_NAME:-}" ]; then
    echo "Error: VPS_DOMAIN_NAME is not set. Please set it in your environment."
    exit 1
fi

# Check if certificate already exists
if [ ! -d "/etc/letsencrypt/live/$VPS_DOMAIN_NAME" ]; then
    echo "Notice: SSL certificate for $VPS_DOMAIN_NAME not found."
    
    if [ -n "${VPS_ADMIN_EMAIL:-}" ]; then
        echo "Attempting to generate initial SSL certificate for $VPS_DOMAIN_NAME..."

        if { [ -n "${DNS_PROVIDER:-}" ] && [ -z "${DNS_API_TOKEN:-}" ]; } ||
           { [ -z "${DNS_PROVIDER:-}" ] && [ -n "${DNS_API_TOKEN:-}" ]; }; then
            echo "Error: DNS_PROVIDER and DNS_API_TOKEN must be configured together." >&2
            exit 1
        fi

        if [ -n "${DNS_PROVIDER:-}" ]; then
            case "$DNS_PROVIDER" in
                *[!a-z0-9-]*)
                    echo "Error: DNS_PROVIDER must use lowercase letters, digits, or '-'." >&2
                    exit 1
                    ;;
            esac
            echo "DNS provider ($DNS_PROVIDER) configured. Using DNS-01 challenge for wildcard certificate..."
            
            mkdir -p /root/.secrets/certbot
            chmod 700 /root/.secrets/certbot
            CRED_FILE="/root/.secrets/certbot/${DNS_PROVIDER}.ini"
            
            # Map provider to plugin name and credential format
            case "$DNS_PROVIDER" in
                aliyun)
                    case "$DNS_API_TOKEN" in
                        ?*:?*) ;;
                        *)
                            echo "Error: Aliyun credentials must be AccessKeyID:AccessKeySecret." >&2
                            exit 1
                            ;;
                    esac
                    _plugin="dns-aliyun"
                    {
                        printf 'dns_aliyun_access_key = %s\n' "${DNS_API_TOKEN%:*}"
                        printf 'dns_aliyun_access_key_secret = %s\n' "${DNS_API_TOKEN#*:}"
                    } > "$CRED_FILE"
                    ;;
                dnspod|tencent)
                    case "$DNS_API_TOKEN" in
                        ?*,?*) ;;
                        *)
                            echo "Error: DNSPod credentials must be API_ID,API_Token." >&2
                            exit 1
                            ;;
                    esac
                    _plugin="dns-dnspod"
                    {
                        printf 'dns_dnspod_api_id = %s\n' "${DNS_API_TOKEN%,*}"
                        printf 'dns_dnspod_api_token = %s\n' "${DNS_API_TOKEN#*,}"
                    } > "$CRED_FILE"
                    ;;
                cloudflare)
                    _plugin="dns-cloudflare"
                    printf 'dns_cloudflare_api_token = %s\n' "$DNS_API_TOKEN" > "$CRED_FILE"
                    ;;
                *)
                    _plugin="dns-$DNS_PROVIDER"
                    printf 'dns_%s_api_token = %s\n' \
                        "$DNS_PROVIDER" "$DNS_API_TOKEN" > "$CRED_FILE"
                    ;;
            esac
            
            chmod 600 "$CRED_FILE"
            
            certbot certonly \
                --authenticator "$_plugin" \
                --"$_plugin"-credentials "$CRED_FILE" \
                -d "$VPS_DOMAIN_NAME" \
                -d "*.$VPS_DOMAIN_NAME" \
                --non-interactive \
                --agree-tos \
                -m "$VPS_ADMIN_EMAIL" || {
                echo "Error: certificate issuance with $DNS_PROVIDER DNS failed." >&2
                exit 1
            }
        else
            # HTTP-01 can issue only the apex certificate. Wildcards need DNS-01.
            echo "Using standalone HTTP-01 for $VPS_DOMAIN_NAME (no wildcard)."
            certbot certonly \
                --standalone \
                -d "$VPS_DOMAIN_NAME" \
                --non-interactive \
                --agree-tos \
                --pre-hook "systemctl stop nginx.service" \
                --post-hook "systemctl start nginx.service" \
                -m "$VPS_ADMIN_EMAIL" || {
                echo "Error: standalone certificate issuance failed; ensure port 80 is reachable." >&2
                exit 1
            }
        fi
    else
        echo "VPS_ADMIN_EMAIL is not set. Skipping automatic Let's Encrypt certificate generation."
        echo "You can generate it manually later using:"
        echo "  certbot certonly --standalone -d $VPS_DOMAIN_NAME"
    fi
else
    echo "SSL certificate for $VPS_DOMAIN_NAME already exists."
fi

# Setup automatic renewal hooks
echo "Configuring Certbot renewal hooks for Nginx..."
mkdir -p /etc/letsencrypt/renewal-hooks/deploy
mkdir -p /etc/letsencrypt/renewal-hooks/pre
mkdir -p /etc/letsencrypt/renewal-hooks/post

# 1. Deploy Hook: Always reload Nginx when a certificate is successfully renewed (Zero Downtime)
cat > /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh <<'EOF'
#!/bin/sh
systemctl reload nginx.service
EOF
chmod +x /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh

# Standalone certificates persist their pre/post hooks in their renewal
# configuration. Remove the old global hooks, which had no lineage variable and
# therefore could not decide which authenticator was being renewed: certbot
# exports RENEWAL_LINEAGE to deploy hooks only.
rm -f /etc/letsencrypt/renewal-hooks/pre/stop-nginx-if-standalone.sh
rm -f /etc/letsencrypt/renewal-hooks/post/start-nginx-if-standalone.sh

# A standalone lineage issued before that change carries no hooks of its own,
# and has just lost the global ones, so its next unattended renewal would fail
# against an nginx still holding port 80. certbot writes [renewalparams] as the
# final section of a renewal file, so appending lands inside it.
for _renewal in /etc/letsencrypt/renewal/*.conf; do
    [ -f "$_renewal" ] || continue
    grep -q '^authenticator = standalone' "$_renewal" || continue
    if grep -q '^pre_hook = ' "$_renewal"; then
        continue
    fi
    printf 'pre_hook = systemctl stop nginx.service\n' >> "$_renewal"
    printf 'post_hook = systemctl start nginx.service\n' >> "$_renewal"
    echo "Adopted standalone renewal hooks into $_renewal"
done

# Ensure certbot timer is active
systemctl enable --now certbot.timer
echo "Certbot automatic renewal configured."
