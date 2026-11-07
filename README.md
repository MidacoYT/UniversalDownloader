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
