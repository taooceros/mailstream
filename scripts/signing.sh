#!/bin/zsh
# Creates the self-signed "Mailstream Local Signing" code-signing identity in the login keychain.
# Builds signed with it keep one code identity, so the Keychain item Mailstream creates (OAuth
# tokens) stays readable across rebuilds; macOS still asks once per build, since a self-signed
# build has no Team ID (an Apple Development certificate avoids that). Creating the identity asks
# once for your password to trust the certificate for code signing.
set -euo pipefail
NAME="Mailstream Local Signing"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
if security find-identity -v -p codesigning | grep -q "\"$NAME\""; then
  echo "$NAME is already installed."
  exit 0
fi
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT
cat > "$DIR/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF
# macOS's LibreSSL writes a PKCS#12 file `security import` reads.
OPENSSL=/usr/bin/openssl
$OPENSSL req -x509 -newkey rsa:2048 -nodes -days 7300 -config "$DIR/cert.cnf" \
  -keyout "$DIR/key.pem" -out "$DIR/cert.pem" 2>/dev/null
PASS=$($OPENSSL rand -hex 16)
$OPENSSL pkcs12 -export -inkey "$DIR/key.pem" -in "$DIR/cert.pem" -out "$DIR/identity.p12" \
  -passout "pass:$PASS" 2>/dev/null
security import "$DIR/identity.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign >/dev/null
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$DIR/cert.pem"
echo "Installed $NAME."
