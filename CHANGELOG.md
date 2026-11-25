# История версий лаунчера COLDZ

Новые версии сверху. Первая строка записи: текст для `npm run release -- -Notes "…"` (он попадает в `latest.json`
и в описание выпуска на GitHub), ниже подробности. Порядок выпуска: README, раздел 5, и `docs/ПРАВИЛА.md`.

## 0.5.10

- «Играть» больше не запускает FiveM так, как игра не разрешает: если ссылку fivem:// забрал лаунчер другого проекта, лаунчер скажет об этом и подскажет адрес, а не уронит игру.
- EN: Play no longer starts FiveM in a way the game refuses: if another project's launcher took over the fivem:// link, the launcher says so and shows the address instead of crashing the game.
- ES: Jugar ya no inicia FiveM de una forma que el juego rechaza: si el launcher de otro proyecto se quedó con el enlace fivem://, el launcher lo dice y muestra la dirección en vez de tumbar el juego.
- PT-BR: Jogar já não inicia o FiveM de um jeito que o jogo recusa: se o launcher de outro projeto ficou com o link fivem://, o launcher avisa e mostra o endereço em vez de derrubar o jogo.
- Зачем: крашдамп игрока 25.11 (`adhesive.dll+DC52AF`, окно «This application should be launched directly from the shell
  or a web browser»). У «Играть» был запасной путь `FiveM.exe "<ссылка>"`, когда обработчик `fivem://` чужой и починить
  его не вышло. Игра так не стартует: падает и оставляет открытой сессию Cfx, из-за чего следующий вход даёт
  «Your account is already being used». В правилах это и было записано («FiveM.exe с параметрами нельзя»), но запасной
  путь остался с ранних версий.
- `lib.rs play`: путь один — отдать ссылку `explorer.exe`. Не вышло: строка в `fivem-search.log` (свой обработчик или
  чужой) и ошибка `link_blocked` окну. Окно показывает, что делать: запустить FiveM самому и войти `connect <адрес>`
  (или «Скопировать адрес»). Тексты на 4 языках, без длинных тире.

## 0.5.9

- «Играть» теперь проверяет, что открывает настоящий FiveM: клиент другого проекта под именем FiveM.exe больше не уведёт вас на чужой сервер.
- EN: Play now checks that it opens the real FiveM: another project's client named FiveM.exe can no longer take you to someone else's server.
- ES: Jugar ahora comprueba que abre el FiveM real: el cliente de otro proyecto llamado FiveM.exe ya no puede llevarte a un servidor ajeno.
- PT-BR: Jogar agora verifica se abre o FiveM real: o cliente de outro projeto chamado FiveM.exe já não pode levá-lo a um servidor alheio.
- Зачем: 0.5.8 сверял обработчик `fivem://` с найденным FiveM.exe (`same_exe`), но поиск начинается с той же записи в реестре.
  Если клиент другого проекта тоже зовётся FiveM.exe, находка и обработчик совпадают — и проверка молчит. Имя файла ничего
  не доказывает, поэтому теперь спрашиваем сам файл.
- `fivem.rs trust_of`: у найденного FiveM.exe читаем сведения о версии (`CompanyName` = Cfx.re, `OriginalFilename` =
  CitizenMP.exe — внутреннее имя FiveM) и проверяем подпись Authenticode (настоящий клиент подписан Rockstar Games через
  DigiCert). Вместе этого достаточно: сведения о версии подделать можно, но изменённый клиент не сохранит подпись Rockstar.
  Субъект сертификата не читаем — ради этого не стоит тянуть весь CryptQueryObject.
- `fivem.rs search`: подлинный клиент выигрывает сразу, как только найден. Подлинного нет — работает прежний порядок
  источников, но похожий на FiveM предпочитается чужому клиенту, и причина идёт в `fivem-search.log`. Жёсткого отказа нет
  намеренно: на старой Windows без свежих корней подпись честного игрока может не провериться, и это не повод не пустить его
  в игру. Выбор игрока вручную главнее любой проверки.
- Подпись проверяется без сети (`WTD_REVOKE_NONE`, `WTD_CACHE_ONLY_URL_RETRIEVAL`): игрок у провайдера, который режет
  зарубежные сервисы, не должен ждать запроса к списку отзыва — ровно для таких игроков лаунчер и нужен.
- Тесты (`fivem.rs`, через `Sources`): подлинный бьёт чужого, найденного первым; без подлинного похожий бьёт чужого; один
  чужой всё равно запускается; ручной выбор не переопределяется; на машине с установленным FiveM настоящий клиент
  распознаётся как подлинный.

## 0.5.8

- Новости и новые версии лаунчер теперь проверяет сам, даже если он свёрнут в трей. «Играть» открывает именно ваш FiveM, а не клиент другого проекта.
- EN: The launcher now checks news and new versions by itself, even when it is hidden in the tray. Play opens your own FiveM, not another project's client.
- ES: El lanzador ahora comprueba las noticias y las versiones nuevas por sí solo, aunque esté en la bandeja. Jugar abre tu propio FiveM, no el cliente de otro proyecto.
- PT-BR: O launcher agora verifica as notícias e as versões novas sozinho, mesmo na bandeja. Jogar abre o seu próprio FiveM, não o cliente de outro projeto.
- Подробнее: `ui/app.js refreshFeed`: новости раз в 30 минут и при открытии окна (если старше 10 минут), обновление раз в 3 часа и при
  открытии окна (если старше 30 минут). До 0.5.8 спрашивали только при запуске, и в трее новая версия не появлялась.
- `fivem.rs ensure_link_handler`: обработчик fivem:// доверяем, только если он ведёт на найденный FiveM.exe (`same_exe`); чужой
  переписываем на наш для текущего пользователя, строка в `fivem-search.log` (24.11: игроков уводило на другой сервер).

## 0.5.7

- «Играть» теперь всё решает сама: если вход в FiveM у провайдера не отвечает, лаунчер без вопросов подключает через сервер COLDZ. Windows спросит разрешение один раз: нажмите «Да».
- EN: Play now decides everything by itself: if the FiveM sign-in does not answer at your provider, the launcher connects through the COLDZ server without any questions. Windows asks for permission once: press "Yes".
- ES: Jugar ahora lo decide todo solo: si el inicio de sesión de FiveM no responde con tu proveedor, el lanzador conecta por el servidor COLDZ sin preguntas. Windows pide permiso una vez: pulsa «Sí».
- PT-BR: Jogar agora decide tudo sozinho: se o login do FiveM não responde no seu provedor, o launcher conecta pelo servidor COLDZ sem perguntas. O Windows pede permissão uma vez: clique em «Sim».
- Подробнее: план «Играть» считает проверка связи (`netcheck.rs plan`, README 7б): вход через сервер COLDZ включается сам, ход
  по шагам под кнопкой; отказ и ошибки — карточка с одним текстом, «Попробовать ещё раз» и «Играть без этого». Карточка
  «Связь с серверами»: одна главная кнопка и «Закрыть», список под «Подробнее». Имена: «Вход через сервер COLDZ», «запасной
  сервер COLDZ», «Вход в FiveM»; слова «маршрут», «релей», Rockstar из текстов окна убраны.
- Одно разрешение Windows вместо двух: повышенная копия ждёт закрытия лаунчера и сама убирает строки из hosts, и при
  закрытии, и при сбое, и при выключении Windows (`hosts.rs guard`, README 7г). Файл hosts меняется побайтно.
- VPN или прокси на ПК: если адреса в разных странах отвечают за 3 мс и быстрее, проверке лаунчер не верит и просит
  выключить (`proxyCheck`, README 7б), пометка в `net-check.log`.
- Через сервер COLDZ идут все группы адресов (вход и службы FiveM), а не только упавшая; группа считается упавшей, если не
  прошёл любой её адрес, обрыв большого ответа на 16 КБ тоже (Александр 23.11, README 7г).
- Пересылка входа: долгие тихие соединения не теряются на роутерах, ответ после конца отправки доходит целиком, длинный
  хендшейк принимается, при молчании релея берётся следующий (`tunnel.rs`, README 7г).

## 0.5.4

- Перед «Играть» лаунчер ставит FiveM сборку игры сервера и сам докачивает файлы игры для сервера (с продолжением после обрыва): они больше не качаются внутри игры.
- EN: Before Play the launcher sets FiveM to the server's game build and downloads the game files for the server itself (resuming after a break): they are no longer downloaded inside the game.
- ES: Antes de Jugar, el lanzador pone en FiveM la versión del juego del servidor y descarga él mismo los archivos del juego para el servidor (continúa tras un corte): ya no se descargan dentro del juego.
- PT-BR: Antes de Jogar, o launcher define no FiveM a versão do jogo do servidor e baixa ele mesmo os arquivos do jogo para o servidor (continua após uma queda): eles não são mais baixados dentro do jogo.
- Подробнее: `vars` из `/info.json` выбранного маршрута (`sv_enforceGameBuild`, `sv_defaultGameBuild`, `sv_pureLevel`,
  `sv_poolSizesIncrease`) → `SavedBuildNumber`, `DefaultBuild`, `PureLevel`, `PoolSizesIncrease` в
  `FiveM.app\CitizenFX.ini`, только если FiveM не запущен и строки отличаются; остальные строки (`IVPath`) и кодировка не
  меняются; ошибки только в `fivem-search.log`. Если файл записан, под «Играть» строка: нажать «Continue» в окне
  обновления данных FiveM и дождаться загрузки (README 7м).
- Файл игры для сервера: `override+update+update.rpf_abf3a580…` (1 423 288 320 байт, `update.rpf` сборки 3258 из
  таблицы FiveM `GameCache.cpp`, адрес CDN Cfx.re) в `FiveM.app\data\game-storage`, если сборка сервера младше exe и
  файла нет; часть `.coldz-part` продолжается по `Range`, SHA-1 перед переименованием, прогресс и «Отменить» под
  «Играть», «Играть без докачки» (README 7н). Таблица: `launcher.config.json → gameStorage`, `news.json` главнее.

## 0.5.3

- Убран график онлайна; кнопки Instagram и TikTok по языку; новости ALPHA 5.2.
- EN: Online graph removed; Instagram and TikTok buttons follow the window language; ALPHA 5.2 news.
- ES: Se quitó la gráfica de jugadores en línea; botones de Instagram y TikTok según el idioma; noticias de ALPHA 5.2.
- PT-BR: Gráfico de jogadores online removido; botões de Instagram e TikTok conforme o idioma; notícias da ALPHA 5.2.
- Подробнее: линия онлайна за сутки в ячейке «Игроки» убрана (Александр 20.11), число «N / 48», пинг и «Перезапуск через
  N мин» остались (README 7ж). Ссылки `links` с полем `langs`: Instagram `coldz_gta5_dayz` (ru, es), `_en` (en), `_br`
  (pt-BR); TikTok `@coldz_gta5` (ru, es), `_en` (en), `_br` (pt-BR). Новости ALPHA 5.2 приходят из `news.json`.

## 0.5.2

- Лаунчер больше не закрывается молча при запуске: если на компьютере нет компонента Windows WebView2, появится окно со ссылкой на его установку, а при другой ошибке запуска окно с текстом ошибки, который можно прислать в группу.
- Подробнее: проверка WebView2 до создания окна (`startup.rs`), журнал `start-error.log` в папке настроек, README 7л.

## 0.5.1

- Исправлен запуск: 0.5.0 падал сразу после старта, ещё до появления окна (состояние обновления регистрировалось дважды,
  Tauri 2.12 на этом останавливает программу). Больше ничего не менялось.

## 0.5.0

Отчёт для поддержки одной кнопкой, подсказки после вылетов, запасной адрес новостей и обновлений, перезапуск сервера в окне

- «Настройки» (шестерёнка): «Собрать отчёт для поддержки»: список файлов до создания, zip на рабочем столе (журналы
  лаунчера и FiveM, сведения о Windows и папках игры), имя пользователя, адреса и почта скрыты, дамп вылета по
  галочке; ничего не отправляется (README 7д).
- После вылета FiveM одна строка с вероятной причиной: повреждён файл игры, мало места, две копии GTA 5, OneDrive,
  медленный диск (README 7д).
- `news.json` и обновления ещё и с `https://coldz.vip/launcher/`; `news.json` подписан своим ключом, без подписи не
  берётся (README 7е). Подготовка к закрытию репозитория (README 7к).
- «Перезапуск через N мин» и вопрос по «Играть» за 2 минуты; линия онлайна за сутки; пинг основного сервера и
  резервных маршрутов в карточке связи (сервер: winter-server `feat/restart-var-1811`, README 7ж).
- Журнал связи: каждый релей входа с номером, причиной остановки и байтами; работа пересылки (README 7г).
- Обновление скачивается заранее и ставится тихо при закрытии (README 5); портативная версия `.zip` (README 7и).
- Окно запоминает размер и место, помещается на ноутбуках при масштабе 125-150 %, по желанию сворачивается в трей
  (README 7й).

## 0.4.0

Вход в FiveM из России без VPN: лаунчер по кнопке ведёт вход Rockstar через резервный маршрут COLDZ

- Проверка связи теперь спрашивает и адреса входа FiveM (Rockstar: `scui.rockstargames.com`,
  `rgl-prod.ros.rockstargames.com`, `prod.ros.rockstargames.com`; и Cfx.re), и релей COLDZ через туннель. В окне
  строки «Вход в FiveM (Rockstar)» и «Резервный маршрут входа» (4 языка).
- Вход не отвечает, релей отвечает: кнопка «Включить резервный маршрут входа». По ней лаунчер слушает 127.0.0.1:443,
  пересылает соединения релею внутри шифрованного туннеля с закреплённым сертификатом и записывает адреса входа в
  файл hosts Windows между метками (один запрос Windows «от имени администратора»). Выключение и закрытие лаунчера
  убирают строки (ещё один запрос). Подробно README, раздел 7г.
- `news.json` → `signin` (группы адресов и релеи с отпечатками): меняется без нового выпуска. Релеи: `relay/tunnel/`.
- Запас на случай вылета: при запуске лаунчер подхватывает или убирает свой старый блок в hosts.
- Туннель поставлен на релеях Варшава и Алматы 18.11 (порт 8443, `relay/tunnel/`), отпечатки в `news.json`. В карточке
  входа по слову Александра назван VPN как то, что помогает на время входа (только там).

## 0.3.5

Лаунчер сам выбирает рабочий маршрут к серверу, если основной недоступен

- По «Играть» лаунчер до 3 секунд ждёт ответа основного сервера (`serverAddress` из `news.json`). Отвечает:
  подключение напрямую, как раньше. Не отвечает: лаунчер проверяет резервные маршруты по порядку (сейчас Варшава
  `177.3.198.34:30120`, затем Алматы `46.247.40.34:30120`) и подключает к первому, который ответил. Все адреса
  спрашиваются сразу, поэтому ожидание не дольше 3 секунд.
- Список резервных маршрутов: `"relays"` в `news.json` (меняется без нового выпуска); вшитый запасной список:
  `"relays"` в `launcher.config.json`. Пустой список в `news.json` = без резервных маршрутов.
- Маршрут, который сработал в прошлый раз, запоминается (`settings.json` → `lastRoute`) и среди резервных идёт первым;
  основной сервер всё равно проверяется первым каждый раз.
- Под «Сервер онлайн» маленькая строка «Подключение: основной сервер» или «Подключение: резервный маршрут» (4 языка).
  Ник, статус, пинг и адрес для F8 идут по тому же маршруту.
- «Проверить связь» показывает и строку «Резервный маршрут: работает / не отвечает». Основной сервер не отвечает, а
  резервный маршрут работает: карточки «Сервер COLDZ сейчас недоступен» нет, «Связь в порядке: серверы FiveM
  отвечают, к серверу COLDZ лаунчер подключится по резервному маршруту.»
- Проверка маршрута: `GET http://<адрес>/info.json`, ответ 200 и JSON-объект (как первый запрос FiveM при входе).
- «Играть» больше не показывает окно «explorer.exe: Приложение не найдено», если FiveM переставлен в другую папку
  (ветка fix/link-handler-1611). Перед запуском лаунчер проверяет обработчик ссылок fivem://. Нет его или он ведёт
  на FiveM.exe, которого уже нет: обработчик направляется на найденный FiveM.exe (только для текущего пользователя,
  без прав администратора), запись в `fivem-search.log`. Не получилось: FiveM.exe запускается напрямую, без explorer.

## 0.3.4

Проверка связи и при запуске лаунчера; понятный совет, если серверы FiveM недоступны у провайдера; «Всё равно играть» и «Повторить»

- Проверка связи с серверами FiveM (Cfx.re) и сервером COLDZ идёт ещё и один раз при запуске лаунчера, тихо. Если
  недоступно то, без чего FiveM не запустится, под «Играть» вместо «FiveM установлен, можно играть» строка «Серверы
  FiveM недоступны у вашего провайдера. Попробуйте другую сеть (например, мобильный интернет) или повторите позже.»
  и ссылки «Повторить», «Подробнее».
- Сервер COLDZ не отвечает: по «Играть» карточка «Сервер COLDZ сейчас недоступен: возможно, идёт перезапуск.
  Попробуйте через пару минут.»
- Карточка по «Играть»: «Всё равно играть», «Повторить», «Закрыть». «Играть» не блокируется никогда.
- Тексты на 4 языках (RU, EN, ES, PT-BR), RU согласован с Александром. Способы обхода блокировок не упоминаются.
- Ожидание короче: до 5 секунд на адрес (было 8), в худшем случае около 7 секунд. Медленная, но идущая связь (больше
  32 КБ к концу ожидания) больше не считается обрывом.
- Сервер COLDZ проверяется запросом `/info.json`: с него FiveM начинает каждый вход.
- Итог проверки по тому, что раньше остановит игрока: нет интернета, Cfx.re режется, сервер COLDZ не отвечает, режется
  только список серверов FiveM («Часть серверов FiveM недоступна у вашего провайдера, но войти на COLDZ через лаунчер
  это не мешает.»).

## 0.3.3

Перед «Играть» лаунчер проверяет связь с серверами FiveM и сервером COLDZ; кнопка «Проверить связь»

## 0.3.2

Лаунчер находит FiveM в любой папке; если не нашёл, кнопка «Указать FiveM.exe вручную»

## 0.3.1

«Играть» не запускает FiveM второй раз; кнопка «Закрыть FiveM», если он завис

## 0.3.0

Поле «Ник»: ник на сервере COLDZ

## 0.2.1

Исправлен запуск FiveM кнопкой «Играть»

## 0.2.0

Адрес сервера из новостей, вкладка «Правила», вид экрана загрузки сервера

## 0.1.0

Первая версия: запуск FiveM с адресом сервера, статус сервера, новости, ссылки на соцсети, автообновление
