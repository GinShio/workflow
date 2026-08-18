#!/bin/sh
#@tags: usage:vps, scope:apps, os:debian, os:ubuntu
# Apps: Certbot Configuration for VPS

set -e

if [ -z "${VPS_DOMAIN_NAME:-}" ]; then
    echo "Error: VPS_DOMAIN_NAME is not set. Please set it in your environment."
    exit 1
fi

# Check if certificate already exists
if [ ! -d "/etc/letsencrypt/live/$VPS_DOMAIN_NAME" ]; then
    echo "Notice: SSL certificate for $VPS_DOMAIN_NAME not found."
    
    if [ -n "${VPS_ADMIN_EMAIL:-}" ]; then
        echo "Attempting to generate initial SSL certificate for $VPS_DOMAIN_NAME..."
        
        if [ -n "${DNS_PROVIDER:-}" ] && [ -n "${DNS_API_TOKEN:-}" ]; then
            echo "DNS provider ($DNS_PROVIDER) configured. Using DNS-01 challenge for wildcard certificate..."
            
            mkdir -p /root/.secrets/certbot
            CRED_FILE="/root/.secrets/certbot/${DNS_PROVIDER}.ini"
            
            # Map provider to plugin name and credential format
            case "$DNS_PROVIDER" in
                aliyun)
                    _plugin="dns-aliyun"
                    echo -e "dns_aliyun_access_key = ${DNS_API_TOKEN%:*}" > "$CRED_FILE"
                    echo -e "dns_aliyun_access_key_secret = ${DNS_API_TOKEN#*:}" >> "$CRED_FILE"
                    ;;
                dnspod|tencent)
                    _plugin="dns-dnspod"
                    echo -e "dns_dnspod_api_id = ${DNS_API_TOKEN%,*}" > "$CRED_FILE"
                    echo -e "dns_dnspod_api_token = ${DNS_API_TOKEN#*,}" >> "$CRED_FILE"
                    ;;
                cloudflare)
                    _plugin="dns-cloudflare"
                    echo "dns_cloudflare_api_token = $DNS_API_TOKEN" > "$CRED_FILE"
                    ;;
                *)
                    _plugin="dns-$DNS_PROVIDER"
                    echo "dns_${DNS_PROVIDER}_api_token = $DNS_API_TOKEN" > "$CRED_FILE"
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
                echo "Warning: Failed to generate certificate using $DNS_PROVIDER DNS."
                DNS_PROVIDER="" # Trigger fallback
            }
        fi
        
        # If DNS provider wasn't set, or plugin installation failed, fallback to standalone
        if [ -z "${DNS_PROVIDER:-}" ]; then
            echo "No DNS provider configured or plugin failed. Using standalone HTTP-01 challenge..."
            
            # Stop Nginx to free up port 80 for standalone challenge
            echo "Stopping Nginx temporarily for Certbot standalone..."
            systemctl stop nginx.service || true
            
            certbot certonly \
                --standalone \
                -d "$VPS_DOMAIN_NAME" \
                -d "*.$VPS_DOMAIN_NAME" \
                --non-interactive \
                --agree-tos \
                -m "$VPS_ADMIN_EMAIL" || {
                echo "Warning: Failed to generate certificate using standalone mode. Ensure port 80 is open."
            }
            
            echo "Starting Nginx back up..."
            systemctl start nginx.service || true
        fi
    else
        echo "VPS_ADMIN_EMAIL is not set. Skipping automatic Let's Encrypt certificate generation."
        echo "You can generate it manually later using:"
        echo "  sudo certbot certonly --standalone -d $VPS_DOMAIN_NAME -d *.$VPS_DOMAIN_NAME"
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

# 2. Pre/Post Hooks: Only needed for standalone mode to free port 80.
# We check if the renewal is using standalone before stopping Nginx.
cat > /etc/letsencrypt/renewal-hooks/pre/stop-nginx-if-standalone.sh <<'EOF'
#!/bin/sh
# RENEWED_DOMAINS is populated by certbot, but we can also just check the authenticator
if grep -q "authenticator = standalone" "$RENEWAL_LINEAGE"; then
    systemctl stop nginx.service
fi
EOF
chmod +x /etc/letsencrypt/renewal-hooks/pre/stop-nginx-if-standalone.sh

cat > /etc/letsencrypt/renewal-hooks/post/start-nginx-if-standalone.sh <<'EOF'
#!/bin/sh
if grep -q "authenticator = standalone" "$RENEWAL_LINEAGE"; then
    systemctl start nginx.service
fi
EOF
chmod +x /etc/letsencrypt/renewal-hooks/post/start-nginx-if-standalone.sh

# Ensure certbot timer is active
systemctl enable --now certbot.timer || true
echo "Certbot automatic renewal configured."
