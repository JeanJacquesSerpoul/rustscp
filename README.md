# RustSCP — client SCP/SFTP pour Linux et Windows

[![CI](https://github.com/JeanJacquesSerpoul/rustscp/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/JeanJacquesSerpoul/rustscp/actions/workflows/ci.yml)

[English](README.en.md) · **Français**

Client graphique en **Rust · Tauri · Leptos**, inspiré de FileZilla, conçu pour les serveurs qui exigent
**une clé SSH et un code TOTP en même temps**
(`AuthenticationMethods publickey,keyboard-interactive` + `pam_google_authenticator`),
ce que FileZilla ne gère pas.

- cœur réseau en Rust natif (`russh`, `russh-sftp`) : SSH, SCP, SFTP, file de transferts ;
- interface en Leptos compilée en WebAssembly, affichée par Tauri 2 (WebKitGTK) ;
- types et utilitaires partagés entre les deux (`shared/`).

## Installation

Des paquets prêts à l'emploi (`.deb`, AppImage, binaire seul) sont joints à chaque
[release](https://github.com/JeanJacquesSerpoul/rustscp/releases).

Pour compiler depuis les sources :

```bash
./install.sh
```

Le script compile l'application en mode release, installe `rustscp` dans
`~/.local/bin` et ajoute une entrée « RustSCP » dans le menu des applications.
Paquets `.deb` / AppImage : `cargo tauri build`.

Prérequis : Rust stable, cible `wasm32-unknown-unknown`, `trunk`, `tauri-cli` 2,
WebKitGTK 4.1 et OpenSSL (Debian : `libwebkit2gtk-4.1-dev libssl-dev libdbus-1-dev`).

Développement : `cargo tauri dev`.

### Windows

Dans PowerShell, à la racine du dépôt :

```powershell
powershell -ExecutionPolicy Bypass -File build-windows.ps1
```

Le script produit `target\release\rustscp.exe` (exécutable autonome) et un
installateur NSIS dans `target\release\bundle\nsis\` ; `-NoInstaller` ne produit
que l'exécutable.

Prérequis : Rust stable (MSVC) avec les Build Tools de Visual Studio (charge de travail
« Développement Desktop en C++ »), cible `wasm32-unknown-unknown`, `trunk`, `tauri-cli` 2.
WebView2 est fourni avec Windows 10/11. NASM est facultatif (des objets précompilés
d'`aws-lc-sys` sont utilisés à défaut).

Sur Windows, les secrets sont rangés dans le gestionnaire d'identification, la
configuration dans `%APPDATA%\rustscp\`, et l'agent SSH utilisé est celui d'OpenSSH
pour Windows (à défaut, Pageant). La racine « / » du panneau local liste les lecteurs.

## Langues

L'interface et les messages sont disponibles en **français** et en **anglais**.
Par défaut, la langue suit celle du système (`LANG`, `LC_MESSAGES`…) : français si
elle commence par `fr`, anglais sinon. Elle peut être forcée dans **Paramètres → Langue**.

## Authentification multi-facteurs

Le moteur suit les étapes imposées par le serveur :

1. **Clé publique** : fichier de clé (ed25519, ECDSA, RSA, chiffrée ou non, au format
   OpenSSH, PEM ou PuTTY `.ppk` v2/v3, certificat `*-cert.pub` détecté) et/ou clés de
   l'agent SSH (ssh-agent ; sous Windows, agent OpenSSH ou Pageant). Le serveur répond
   « succès partiel ».
2. **keyboard-interactive** : chaque question du serveur est classée (code TOTP,
   mot de passe, autre) :
   - code TOTP **demandé** dans une fenêtre dédiée, ou
   - code TOTP **généré automatiquement** à partir du secret enregistré
     (base32 ou URI `otpauth://` du QR code). Le gestionnaire de sites affiche le code
     courant pour vérifier qu'il correspond à votre application.
3. Mot de passe si le serveur l'exige (seul, ou avant/après le TOTP).

Le journal indique chaque étape (« Étape 1 réussie : clé acceptée (authentification
partielle), second facteur requis »…).

Un code déjà utilisé n'est jamais renvoyé : si une reconnexion a lieu dans la même
fenêtre de 30 s, RustSCP attend le code suivant (compatible `DISALLOW_REUSE`).

### Un seul code TOTP, plusieurs transferts parallèles

FileZilla ouvre une connexion par transfert simultané : avec un TOTP, il faudrait un
nouveau code pour chacune. RustSCP **multiplexe** : la navigation et les N transferts
parallèles sont des canaux SSH distincts sur **une seule connexion authentifiée**.

## Fonctionnalités

- Gestionnaire de sites (dossiers, duplication), connexion rapide
  (`utilisateur@hôte:port`) avec historique, onglets multiples.
- Secrets (mot de passe, phrase de passe, secret TOTP) dans le **trousseau du système**
  (Secret Service / KWallet), jamais en clair sur le disque.
- Vérification des clés d'hôte (`~/.ssh/known_hosts` + `~/.config/rustscp/known_hosts`),
  alerte en cas de changement.
- Protocole **SCP natif** (implémenté directement sur le canal, sans programme externe)
  ou **SFTP** ; navigation via SFTP, ou via commandes shell (`find`) sur les serveurs
  « SCP seul » ; repli automatique en SFTP si le serveur refuse les commandes.
- Panneaux local / distant avec arborescence, filtre instantané (texte ou `*.log`),
  tri, colonnes redimensionnables, fichiers cachés, glisser-déposer dans les deux sens
  (et depuis le gestionnaire de fichiers du bureau), déplacement distant par
  glisser-déposer.
- Opérations distantes : créer dossier/fichier, renommer, supprimer (récursif),
  chmod (récursif), copier le chemin, **afficher/éditer** (téléchargement, ouverture
  avec l'application par défaut, renvoi proposé à l'enregistrement).
- File de transferts : en attente / échecs / réussis, progression, vitesse, temps
  restant, annulation, nouvelle tentative, pause ; politique d'écrasement (demander,
  écraser, si plus récent, si taille différente, renommer, ignorer, « pour toute la file »).
- Reconnexion transparente si la connexion tombe pendant les transferts.
- Thème clair / sombre selon le bureau ; interface en français ou en anglais.

### Raccourcis

| Touche | Action |
| --- | --- |
| Ctrl+S | Gestionnaire de sites |
| Ctrl+T / Ctrl+W | Nouvel onglet / fermer l'onglet |
| F5 | Actualiser |
| Ctrl+R | Reconnecter |
| Ctrl+Maj+D | Déconnecter |
| Retour arrière | Dossier parent |
| Entrée / double-clic | Ouvrir le dossier / transférer le fichier |
| Ctrl+U / Ctrl+D | Envoyer / télécharger la sélection |
| F2, Suppr, F7 | Renommer, supprimer, nouveau dossier (distant) |
| Ctrl+A, lettres | Tout sélectionner, recherche au clavier |
| Ctrl+H | Fichiers cachés |
| Ctrl+Q | Quitter |

## Performances

Mesuré en local (release, serveur Docker) : SCP 211 Mio/s en envoi et 422 Mio/s en
réception, SFTP 253 Mio/s en envoi et 422 Mio/s en réception.

- Listes virtualisées (seules les lignes visibles sont dessinées), tri et filtre en
  Rust/WebAssembly : des dossiers de dizaines de milliers d'entrées restent fluides.
- Tout le réseau est asynchrone (tokio) hors de l'interface ; les listings sont mis
  en cache et le contenu affiché se rafraîchit sans perdre la sélection.
- Fenêtre SSH de 16 Mio par canal, SFTP en pipeline (48 requêtes de 64 Kio en vol,
  lecture et écriture), file de transferts synchronisée de façon incrémentale.

## Tests

Un serveur de test (Docker) exige clé + TOTP :

```bash
tests/server/run.sh
```

```bash
cargo test --workspace
```

L'intégration continue GitHub (`.github/workflows/ci.yml`) vérifie le formatage,
Clippy (natif et wasm32) et lance tous les tests contre ce serveur Docker
(`RUSTSCP_REQUIRE_SERVER=1` : son absence fait alors échouer le test). Sur une
étiquette `v*` ou un lancement manuel, elle produit aussi les paquets `.deb` et AppImage.

Débit (mode release) :

```bash
cargo test --release -p rustscp --test integration bench -- --ignored --nocapture
```

Serveur : `127.0.0.1:2222`, utilisateur `test`, clé `tests/server/test_key`,
secret TOTP `JBSWY3DPEHPK3PXP` (à saisir dans une application d'authentification ou
dans le gestionnaire de sites).

### Interface dans un navigateur (développement)

```bash
cargo run -p rustscp-devbridge   # cœur sur 127.0.0.1:1421
trunk serve                      # interface sur localhost:1420
```

Hors de Tauri, `frontend/devshim.js` relie l'interface à cette passerelle HTTP
(sans effet dans l'application). `RUSTSCP_MEMORY_SECRETS=1` garde les secrets en mémoire.

## Structure

```
shared/      types communs (sites, réglages, transferts, dialogues), utilitaires et i18n
src-tauri/   ssh.rs (connexion + auth multi-facteurs), scp.rs (protocole SCP),
             sftp.rs (SFTP en pipeline), remotefs.rs (SFTP / shell),
             transfer.rs (file parallèle, édition distante), sites.rs, config.rs,
             totp.rs, local.rs, commands.rs (commandes Tauri), ui.rs (événements)
devbridge/   passerelle HTTP de développement (interface dans un navigateur)
frontend/    app.rs, panes.rs (listes et arborescences), queue.rs, dialogs.rs,
             sitemanager.rs, actions.rs, state.rs, layout.rs, icons.rs, styles.css
```

Configuration : `~/.config/rustscp/` (`sites.json`, `settings.json`, `known_hosts`).

## Licence

Distribué sous licence MIT : voir [LICENSE](LICENSE).
