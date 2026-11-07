# Universal Downloader

Application Windows native en Rust pour parcourir les jeux Xbox, télécharger leurs paquets PC et gérer une bibliothèque locale.

## Télécharger

- Dernier installateur Windows : <https://github.com/MidacoYT/UniversalDownloader/releases/latest/download/UniversalDownloader-Setup.exe>
- Toutes les releases : <https://github.com/MidacoYT/UniversalDownloader/releases>
- Code source : <https://github.com/MidacoYT/UniversalDownloader>

Chaque commit poussé sur `main` construit et publie une nouvelle release contenant l’installateur Windows x64. La page `Updates` de l’application vérifie ces releases, valide l’empreinte SHA-256 de l’installateur, puis peut le lancer pour mettre l’application à jour.

## Lancer

```powershell
cargo run --release
```

Le catalogue Xbox s’authentifie auprès du service de catalogue, puis charge les fiches Microsoft avec recherche et filtre de plateforme. Les fiches affichent les informations disponibles et donnent accès à la page Xbox officielle.

La bibliothèque analyse un dossier local choisi avec le sélecteur Windows ou saisi à la main. La page Downloads sépare les transferts en cours, les fichiers terminés et les erreurs, avec jaquettes du catalogue, progression, vitesse, estimation du temps restant, pause, reprise et annulation. Le bouton Download d’un jeu résout le paquet MSIXVC via les API Microsoft et XboxDownload, le télécharge, puis passe l’URL de la clé CIK XboxApi directement à la version personnalisée de XvdTool. La clé est lue en mémoire et aucun fichier `.cik` n’est créé par Universal Downloader. La bibliothèque est rescannée après l’extraction.

Le port cherche `Xvd/` et `OnlineFix/` à côté de l’exécutable, puis dans le projet Rust ; `UD_XVD_TOOL_DIR` et `UD_ONLINEFIX_DIR` peuvent pointer vers d’autres dossiers. XvdTool est publié en binaire autonome, sans runtime .NET à installer. Pendant l’extraction et l’enregistrement, pause, reprise et annulation sont disponibles. Si le manifeste existe, `wdapp.exe register` est lancé sous Developer Mode ; l’état Developer Mode précédent est restauré ensuite. Le résultat d’enregistrement apparaît dans Downloads et la bibliothèque est rescannée après l’opération.

Les diagnostics des téléchargements sont ajoutés à `%APPDATA%\UniversalDownloader\app.log`. Les URL signées sont journalisées sans leur query string ; les réponses HTTP en erreur incluent un extrait du corps pour faciliter le diagnostic.
La bibliothèque rapproche le nom de chaque dossier des titres récupérés dans le catalogue Xbox. Les dossiers reconnus affichent leur jeu et leur éditeur, et un bouton ouvre la fiche correspondante. Les dossiers sans correspondance restent affichés avec leur nom d’origine.

## Proposer l’ajout d’un jeu

Les contributions de jeux se font par [issue GitHub](https://github.com/MidacoYT/UniversalDownloader/issues/new). Indique le nom exact du jeu et la boutique concernée, puis joins les données demandées ci-dessous. L’équipe vérifiera les fichiers avant de mettre à jour le catalogue. Ne joins jamais de mot de passe, cookie, code d’autorisation ou jeton de compte.

### Xbox / Microsoft Store

1. Assure-toi d’avoir accès au jeu sur ton compte Xbox/Microsoft.
2. Utilise [CikExtractor](https://github.com/LukeFZ/CikExtractor) pour extraire la CIK du package installé par l’application Xbox.
3. Crée une issue avec le nom du jeu, son **Product ID Microsoft** (par exemple `9NG07QJNK38J`) et joins le fichier `.cik` extrait. N’envoie pas de clé provenant d’un jeu auquel tu n’as pas accès.

La CIK permet de déchiffrer le package Xbox. Ne publie pas de jeton XboxApi : il n’est pas nécessaire pour que l’équipe traite la proposition.

### Epic Games

Les scripts de `tools` récupèrent le manifeste BuildPatch et les données nécessaires pour reconstruire les URL de chunks.

1. Installe Legendary et connecte-toi au compte qui possède le jeu :

   ```powershell
   python -m pip install legendary-gl
   legendary auth
   ```

2. Exporte les manifestes des jeux Epic accessibles à ce compte. Tu peux les écrire directement dans le dossier de manifestes d’EpicApi :

   ```powershell
   python "tools\export_epic_manifests (1).py" --output "D:\EpicApi\public\manifests"
   ```

   Le script écrit aussi `_resultats.csv`. Vérifie que le manifeste du jeu proposé a le statut `OK`.

3. Vérifie que le jeu figure dans `D:\EpicApi\functions\data\games.json` et que son champ `manifest` correspond au nom du fichier `.manifest`. Puis récupère les URL des chunks :

   ```powershell
   python tools\epic_chunk_urls.py `
     --games-json "D:\EpicApi\functions\data\games.json" `
     --manifest-dir "D:\EpicApi\public\manifests" `
     --output "epic_chunk_urls"
   ```

   Le script demande le code d’autorisation Epic avec une saisie masquée. Il est à usage unique : ne l’ajoute jamais à l’issue. Les fichiers générés comprennent `summary.json`, un `.txt` d’URL par jeu et `chunks\<manifest>.json` avec les identifiants des chunks et leurs CDN.

4. Ouvre une issue en précisant le nom du jeu et son `appName`, puis joins le `.manifest`, son entrée de résumé et le JSON `chunks` correspondant. Ajoute aussi le `.txt` d’URL si les pièces jointes le permettent. N’inclus aucun code d’autorisation, jeton d’accès ou cookie Epic.

Les chemins `D:\EpicApi\...` sont les valeurs par défaut du projet. Si EpicApi se trouve ailleurs, adapte les chemins dans la commande.
