# Onboarding : box internet

1. Demander quelle box (Livebox, Freebox…) et si l'UPnP y est activé.
2. Aucun identifiant. Sans option, le pilote cherche lui-même (SSDP, 3 s), puis écoute les
   annonces de la box (une Livebox ne s'annonce pas souvent hors redémarrage).
3. Pour aller plus vite : reprendre l'URL de description (`…/gatedesc.xml`) de la config HA,
   la mettre dans `location`. Si la box la change à un redémarrage, le pilote la retrouve à
   sa prochaine annonce (journal : « retrouvé à une nouvelle adresse »).
4. Déclarer `kind = "igd"` ; `moli-os check-config`, redémarrer.
5. Tester : `connected`, `external_ip`, `line_down` ont une valeur. Lecture seule.
