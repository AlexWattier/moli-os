# Klipper / Moonraker (pilote natif)

- `crates/moli-moonraker`. Remplace le profil `moonraker` (lecture seule, gardé pour qui
  n'a besoin que de l'état) : il écrit (`control` pause / reprise / annulation, `light`),
  sert la vignette de l'impression comme image de l'appareil et lit chaque tête.
- `fixtures/u1.json` : réponses **réelles** d'un Snapmaker U1 (4 octobre 2026) : liste des
  objets Klipper (sans les macros), `objects/query` des objets lus (têtes réduites aux champs
  lus), métadonnées du dernier fichier, totaux de l'historique.
- `fixtures/k2.json` : réponses **réelles** d'une Creality K2 Combo, pendant une impression
  (29 octobre 2026) : liste des objets Klipper (sans les macros), `objects/query` de
  `print_stats`, `virtual_sdcard`, `display_status`, métadonnées Moonraker du fichier (vides
  du trancheur). Nom de fichier changé, identifiants retirés.
- **Creality K2** : avancement, couches et estimation du trancheur lus dans ce que le firmware
  Creality ajoute à `virtual_sdcard` (`cur_print_data`, `layer`, `layer_count`). Le temps
  restant reste la moyenne trancheur / fichier, comme pour toute imprimante Klipper : il peut
  différer de celui de Creality Print, qui ne prend que le trancheur.
- **Essai sur une K2 Combo** (29 octobre 2026, même impression, au même moment) : avant,
  Moli disait 18 % et 12 h 57 restantes, Creality Print 41 % et 3 h 23. Après : 52 % contre 51 %,
  couche 446 contre 447 sur 1092, 2 h 59 restantes contre 2 h 44 (fin vers 00:52 contre 00:38).
- **Pas encore fait sur la K2** : la vignette (Moonraker n'en trouve pas dans le G-code de
  Creality Print), la caméra (absente de `/server/webcams/list`), la lumière (`output_pin LED`,
  pas encore essayée), le CFS (`box`).
- Les commandes d'impression ont la sémantique `control` : un agent qui les envoie attend
  la validation d'un humain (garde du hub), quelle que soit la pièce.
