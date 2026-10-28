# Tuya (local)

- Aucun code repris : trames 3.3 / 3.4 / 3.5 vérifiées octet par octet contre la sortie
  d'implémentations existantes (localtuya, tinytuya), règle de tinytuya pour les en-têtes de version ; l'import lit le
  compte Tuya déjà lié à HA (`tools/home-assistant/tuya-export.py`).
- Pas de fixture d'appareil : les tests (`src/protocol.rs`) utilisent des trames produites
  par ces implémentations (vecteurs d'interopérabilité), pas des captures.
- Vérifié en vrai : import depuis HA, lecture et écriture locales sur des prises Tuya.
- Sous-appareils d'une passerelle (Zigbee/BLE) **en local** : par la connexion de la
  passerelle, chaque trame portant le `cid` du sous-appareil (son `node_id`) ; états reçus et
  ordres envoyés avec ce `cid` (charges utiles vérifiées contre tinytuya, type « zigbee »).
  Il faut dans `tuya.json` : `node_id` et `parent` (l'id de la passerelle) ; l'import les
  remplit (parent : nommé par le cloud, sinon la passerelle qui partage la clé locale, sinon
  la seule passerelle de la maison) et un `parent` ou un `node_id` écrit à la main est conservé. Un `cid`
  inconnu est écrit dans le journal avec ses DPs, pour le retrouver. Sans passerelle locale,
  le cloud reste la voie (lecture seule).
- Passerelle sans clé dans le cloud : l'import lui donne celle de ses sous-appareils (ils
  partagent la clé de leur passerelle) ; une passerelle (`wg*`, `wfcon`) ou un hub infrarouge
  (`wnykq`) n'est jamais lui-même un sous-appareil, même avec un `node_id`.
- **Vérifié sur du vrai matériel** (28 octobre 2026) : passerelle Tuya Zigbee `wg2` en 3.x
  local, deux sondes température/humidité `wsdcg` (codes `va_temperature`, `humidity_value`,
  `battery_percentage`) lues en local par la passerelle ; interrupteurs `wxkg` et un `kg`
  rattachés ; firmware non relevé.
- Reste : la version 3.1 n'est pas gérée ; l'import dépend encore de HA ; ordres vers un
  sous-appareil pas encore essayés en vrai ; une sonde sur pile ne répond pas à la requête
  d'état : sa valeur arrive à son prochain envoi.
- Limites des sous-appareils en local : la disponibilité est celle de la passerelle (un
  sous-appareil sur pile éteint reste « en ligne » tant que sa passerelle l'est) ; l'accusé
  d'un ordre vient de la passerelle, pas de l'appareil Zigbee : seul l'état renvoyé ensuite
  pour son `cid` prouve que l'ordre a été exécuté ; l'état de chaque sous-appareil n'est
  demandé qu'à la connexion, ensuite seul ce que la passerelle pousse arrive.
