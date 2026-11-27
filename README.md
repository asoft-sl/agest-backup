# AGest Backup

Aplicación de escritorio que guarda **copias de seguridad de las tiendas de [AGest](https://agest.asoft.es) en tu propio equipo**: en un disco local, un disco externo o una carpeta de red.

- Inicia sesión con tu cuenta de AGest desde el navegador (OAuth), sin escribir la contraseña en la app.
- Elige una carpeta y programa las copias de cada tienda: diaria, semanal o mensual, a la hora que quieras y conservando las últimas *N*.
- Cada copia es un `.zip` con todos los datos de la tienda en JSON y sus ficheros (imágenes, logos, justificantes), verificado con SHA-256.
- Desde el panel web (*Configuración → Copias de seguridad*) se ven los equipos, el espacio libre en disco y el historial, y también se pueden programar y pedir copias.
- Funciona en segundo plano desde la bandeja del sistema y puede arrancar con el equipo.
- Se actualiza sola: descarga las versiones nuevas, comprueba su SHA-256 y las instala cuando no hay copias en curso.

**[⬇ Descargar la última versión para Windows](https://github.com/asoft-sl/agest-backup/releases/latest/download/agest-backup.exe)** · [Todas las versiones](https://github.com/asoft-sl/agest-backup/releases)

Está hecha en Rust con [egui](https://github.com/emilk/egui) y la misma paleta que el panel web de AGest.

## Requisitos

- Windows 10/11 (también compila en macOS y Linux).
- Una cuenta de AGest con suscripción activa. Las copias las gestiona el titular de la cuenta.
- Para compilar: [Rust](https://rustup.rs) 1.85 o posterior (edición 2024) y, en Windows, las herramientas de compilación de Visual Studio (MSVC).

## Instalación y uso

1. [Descarga `agest-backup.exe`](https://github.com/asoft-sl/agest-backup/releases/latest/download/agest-backup.exe) (o compílalo, ver más abajo) y ábrelo. No necesita instalación.
2. Pulsa **Iniciar sesión con AGest**. Se abre el navegador: inicia sesión si te lo pide y pulsa **Autorizar**.
3. Elige la **carpeta** donde guardar las copias. Dentro se crea una subcarpeta por tienda.
4. En **Programación**, activa cada tienda y elige cuándo copiarla. También puedes hacerlo desde el panel web: los cambios se sincronizan.

Con **Copiar ahora** se hace una copia en el momento. Al cerrar la ventana la app sigue en la bandeja (se puede desactivar en *Ajustes*). Las copias programadas solo se hacen mientras la app está abierta; si el equipo estaba apagado a la hora prevista, la copia se hace en cuanto se enciende.

### Actualizaciones

La app busca una versión nueva al minuto de arrancar y después cada 6 horas. Si la hay, la descarga junto al ejecutable y comprueba su SHA-256. Se instala sola en cuanto la app está en la bandeja y no hay ninguna copia en curso, y vuelve a arrancar en la bandeja. Con la ventana abierta aparece **Reiniciar y actualizar** en la barra lateral y en *Ajustes → Actualizaciones*, donde también se puede buscar a mano o desactivar la actualización automática. Si sales de la app con una versión ya descargada, se instala para el siguiente arranque.

Para dejar de usarla en un equipo, pulsa **Cerrar sesión** en *Ajustes* o desautorízalo desde el panel web. Las copias que ya están en el disco no se borran.

## Qué contiene cada copia

```text
<carpeta>/
├── 12-mi-tienda/
│   ├── agest-12-mi-tienda-20261126-030000.zip
│   └── ...
└── .agest-backup/index.json     (índice local, oculto)
```

Dentro del `.zip`:

| Ruta | Contenido |
|---|---|
| `manifest.json` | Versión del formato, tienda, fecha, filas por tabla y lista de ficheros con su SHA-256 |
| `datos/<tabla>.json` | Un array JSON por tabla con las filas de la tienda (productos, tickets, facturas, clientes...) |
| `ficheros/<disco>/<ruta>` | Imágenes de productos, logos, justificantes de gastos y documentos de la biblioteca |
| `LEEME.txt` | Explicación breve |

Las credenciales, tokens y secretos (contraseñas, certificados, claves de API) **nunca** se incluyen. Esta versión no restaura copias; los JSON se pueden abrir con cualquier herramienta.

## Línea de comandos

La misma app funciona sin ventana, para servidores o el Programador de tareas de Windows. Usa la misma sesión, carpeta y lógica de copia:

```text
agest-backup login             autoriza este equipo (abre el navegador)
agest-backup folder <ruta>     elige la carpeta de copias
agest-backup status            tiendas, programación y espacio en disco
agest-backup backup [--shop ID | --due]
                               copia una tienda, las que tocan (--due) o todas
agest-backup update            instala la última versión, si hay una nueva
agest-backup logout            cierra la sesión y desautoriza el equipo
```

Por ejemplo, una tarea programada cada hora con `agest-backup backup --due` hace las copias pendientes sin tener la app abierta.

## Compilar

```powershell
cargo build --release
```

El ejecutable queda en `target\release\agest-backup.exe`. En *release* no abre consola; los comandos de línea se enganchan a la consola desde la que se lanzan.

## Desarrollo

```powershell
cargo run                                  # interfaz
cargo test                                 # programación, PKCE, retención, descarga y SHA-256
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Por defecto se conecta a producción (`https://api.asoft.es/api`). Para el backend local, en la pantalla de inicio de sesión pulsa *Servidor → Desarrollo* (`https://agest-backend.test/api`), o arranca la primera vez con `AGEST_API_URL`.

| Variable | Efecto |
|---|---|
| `AGEST_API_URL` | Servidor inicial (solo si aún no hay configuración guardada) |
| `AGEST_BACKUP_UPDATE_URL` | URL alternativa para buscar actualizaciones (pruebas); también activa el actualizador en depuración |
| `AGEST_BACKUP_NO_BROWSER=1` | No abre el navegador al iniciar sesión; imprime el enlace. La pantalla de espera también tiene *Copiar enlace* |

### Cómo funciona

1. **Inicio de sesión**: OAuth 2.0 *authorization code* con PKCE (S256) y redirección *loopback* (RFC 8252). La app abre `https://<panel>/backup/authorize`, el titular autoriza y el navegador vuelve a `http://127.0.0.1:<puerto>/callback`. La app canjea el código por un token de dispositivo `agbk_…` que solo sirve para la API de copias.
2. **Latido** cada minuto (`PUT /api/backup-device/heartbeat`): envía la carpeta, el espacio del disco y lo que ocupan las copias, y recibe las tiendas, su programación y las copias pedidas desde el panel.
3. **Programación**: decide la app con la hora local. Toca copiar si la última copia correcta es anterior a la última franja programada; tras un fallo reintenta a los 30 minutos.
4. **Copia**: el servidor genera el ZIP, la app lo descarga a `….zip.part` calculando el SHA-256, lo compara con el del servidor, lo renombra y confirma la copia. Después aplica la retención: borra las copias más antiguas de esa tienda y avisa al panel.

5. **Actualizaciones**: lee `https://api.github.com/repos/asoft-sl/agest-backup/releases/latest`. Si la etiqueta (`vX.Y.Z`) es mayor que la versión de `Cargo.toml`, descarga el `agest-backup.exe` de la versión a `agest-backup.exe.new` y exige que su SHA-256 coincida con el que calcula GitHub (`digest` del fichero) o, si no, con el `agest-backup.exe.sha256` adjunto. Para instalar renombra el ejecutable en uso a `.old`, pone el nuevo en su sitio y lo arranca con `--updated` (y `--minimized` si estaba en la bandeja). El `.old` se borra al arrancar. En compilaciones de depuración no se actualiza salvo con `AGEST_BACKUP_UPDATE_URL`.

### Publicar una versión

1. Sube `version` en `Cargo.toml` y compila en *release*.
2. Crea una *release* en GitHub con la etiqueta `vX.Y.Z` (no marcada como *pre-release*) y adjunta `agest-backup.exe` (con ese nombre exacto) y `agest-backup.exe.sha256`.
3. Los equipos con la app abierta la instalan solos en las 6 horas siguientes.

### Estructura

| Fichero | Qué hace |
|---|---|
| `src/main.rs` | Arranque: interfaz o línea de comandos |
| `src/api.rs` | Cliente de la API (`/backup/oauth/*`, `/backup-device/*`) |
| `src/oauth.rs` | PKCE, servidor *loopback* y canje del código |
| `src/config.rs` | Configuración local y token en el almacén de credenciales |
| `src/worker.rs` | Hilos de control (latido, programación) y de copias; descarga con SHA-256 |
| `src/schedule.rs` | Cuándo toca la siguiente copia |
| `src/store.rs` | Índice local, retención y espacio en disco |
| `src/app.rs`, `src/theme.rs` | Interfaz y estilo |
| `src/tray.rs` | Icono de la bandeja |
| `src/update.rs` | Actualización automática desde las *releases* de GitHub |
| `src/cli.rs` | Línea de comandos |

## Datos locales y seguridad

- **Configuración** (servidor, carpeta, preferencias): `%APPDATA%\ASoft\AGest Backup\data\config.json`.
- **Token del dispositivo**: en el almacén de credenciales del sistema (Administrador de credenciales de Windows, servicio `agest-backup`), nunca en un fichero.
- El servidor solo guarda el hash del token. Desautorizar el equipo desde el panel lo invalida al momento.
- Los ZIP se guardan **sin cifrar**: protege la carpeta (permisos, BitLocker, disco externo guardado) como protegerías cualquier copia de tus datos.

## Licencia

Software propietario de ASoft SL. Todos los derechos reservados.
