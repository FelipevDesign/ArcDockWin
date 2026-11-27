# Funções extras (fork FelipevDesign)

Todas começam **desligadas** e são ligadas em **Configurações**, nas páginas Efeitos,
Atalhos, Ferramentas, Janelas e Pré-visualização. Função desligada não cria janelas,
timers nem ganchos, e não guarda nada na memória.

## Efeitos
- **Estilo do fundo**: Sistema, Escuro, Claro ou Mínimo (sem fundo, só os ícones).
- **Opacidade dos indicadores** de app aberto.
- **Luz sob o app em foco**, com intensidade ajustável.
- **Cor dinâmica na borda**: usa a cor do ícone do app em foco, com intensidade ajustável.
- **Dock em ilhas**: um fundo para cada grupo entre separadores, com o espaço ajustável.

## Atalhos
- **Corrigir atalhos quando um app atualiza**: se o caminho fixado sumiu porque a pasta
  mudou de versão (`Discord\app-1.0.9270`, `WindowsApps\Claude_6.x...`), procura a versão
  nova e corrige o `pinned.txt` (uma cópia fica em `pinned.txt.antes-da-correcao`).
- **Soltar arquivos no dock**: sobre um app, abre os arquivos com ele; fora dos ícones,
  fixa os programas, atalhos e pastas soltos.
- **Pastas fixadas abrem como pilha** (o conteúdo acima do ícone).
- Botão direito num app fixado: **Editar atalho…** (nome, parâmetros, ícone),
  **Adicionar a um grupo** e **Adicionar a um modo de trabalho**.

Formato das linhas no `pinned.txt` (o `|` separa as partes):

```
C:\Caminho\app.exe | --parametros | Nome no dock
::grupo Design | C:\...\a.exe | C:\...\b.exe
::modo Trabalho | C:\...\a.exe | C:\...\b.exe ; --parametros
```

Grupo: o clique mostra os apps do grupo. Modo de trabalho: o clique abre todos.

## Ferramentas (ficam antes de Downloads e Lixeira)
- **Prateleira**: não fica no dock à toa: solte arquivos (que não sejam programas) no fundo
  do dock e ela aparece com eles; esvaziou, some. Guarda só o caminho. Clique abre a lista;
  botão direito num item tira da prateleira; "Copiar todos" para colar no Explorador.
- **Histórico da área de transferência**: só texto, só na memória, com limite ajustável.
  Pausar, limpar e fixar (botão direito no item). Ignora o que gerenciadores de senha
  marcam para ficar fora do histórico.
- **Coletor de cores**: clique na ferramenta e depois em qualquer ponto da tela; copia em
  HEX ou RGB. Botão direito mostra as cores recentes.
- **Timer de foco**: duração ajustável, mostra os minutos restantes no ícone e avisa com som.
- **Música ao lado do dock**: um quadro à parte com capa, título, artista, anterior /
  tocar / próxima e a barra da faixa andando (clique nela para pular). Clique na capa
  abre o app que está tocando.
- As ferramentas também ligam e desligam no botão direito do dock → **Ferramentas no dock**.

## Janelas
- **Zona ao arrastar com Shift**: a área de trabalho menos as margens (padrão igual ao
  modelo do FancyZones: 31, 1, 25 e 32 px). Desligue o FancyZones para não brigarem.
- **Sombra extra na janela encaixada**, com intensidade e suavidade ajustáveis. Vale para
  qualquer janela que esteja na zona (encaixada pelo dock ou pelo FancyZones). Some quando
  a janela minimiza, maximiza, sai da zona ou fecha.

## Pré-visualização
Selecione um arquivo no Explorador e aperte **Espaço** (ou Ctrl+Espaço, na opção).
Esc ou Espaço fecham; ← → trocam de arquivo; Enter abre no programa padrão; Ctrl+C copia.
Mostra imagens, texto e código, pastas, .zip, vídeo e áudio, e usa os mesmos
pré-visualizadores do Explorador para PDF, Office etc. A janela é um processo à parte,
que só existe enquanto está aberta. Enquanto o Peek do PowerToys estiver rodando,
o Espaço fica com ele.


## Adicionadas depois
- **Progresso nos ícones** (Efeitos): a barrinha que os apps mandam para a barra de tarefas
  (downloads, cópias, exportações) aparece no pé do ícone: verde/cor de destaque, amarela
  quando pausada, vermelha com erro. Funciona porque a barra de tarefas do Windows está
  escondida pelo dock: ele se registra como destino desse progresso, e devolve ao sair.
- **Separar perfis do mesmo app** (Atalhos e mouse): janelas de processos abertos com
  `--user-data-dir` vão para o fixado com a mesma pasta de perfil (nos parâmetros do
  atalho ou da linha do `pinned.txt`), ou viram um item à parte. Ex.: Claude e Claude2.
- **Sacudir o mouse aumenta o cursor** (Atalhos e mouse): tamanho e sensibilidade
  ajustáveis; não precisa de administrador. Feche o Shake to Find Cursor antes.

## Barra do topo
Módulos novos (Configurações › Barra do topo, cada um com o seu interruptor, zona e ordem):
- **Menu do sistema** (logo do Windows na ponta esquerda): Sobre este PC, Configurações,
  Microsoft Store, Itens recentes (apps e arquivos), Forçar encerramento… (quem não
  responde aparece primeiro, com confirmação), Suspender, Reiniciar…/Desligar… (com
  confirmação), Bloquear tela e Sair da conta.
- **App em foco**, em negrito: nova janela, minimizar, fechar janela, mostrar no
  Explorador, encerrar o app e forçar encerramento.
- **Área de trabalho virtual**: o nome da atual; clique lista todas, a roda do mouse troca.
- **Ferramentas**: histórico da área de transferência, coletor de cores e timer de foco
  (com os minutos que faltam), as que estiverem ligadas em Ferramentas.
- **Captura de tela**: área, janela, tela inteira e gravação, pela Ferramenta de Captura.
- **Clima** (Open-Meteo, sem conta): agora, sensação, umidade, vento, chance de chuva, UV,
  nascer/pôr do sol, as próximas 6 horas e 7 dias. A última leitura e o lugar ficam
  guardados, então a barra já abre com o clima; atualiza a cada 30 min, ao voltar da
  suspensão e ao abrir o painel (se passou de 10 min). Busca de cidade com lista para escolher.
- **Próximo compromisso**: cole o endereço secreto iCal da agenda no painel; atualiza a cada
  15 min e avisa (bipe + painel aberto) N minutos antes.
- **Bateria**: % e, na opção, o tempo restante; o clique mostra o modo de energia.
- **Notificações**: sino com o número de avisos (lua com o Não incomodar ligado); abre o Win+N.

Páginas **Barra: visual** (fundo transparente, igual ao dock ou personalizado com cor,
opacidade, desfoque, linha e cantos; cor do texto; sombra no texto; cor dinâmica na linha;
ocultar automaticamente) e **Barra: recursos** (velocidade da rede no painel de desempenho,
Não incomodar na Central de Controle, relógios mundiais no calendário, timer de foco liga
o Não incomodar, aviso antes do compromisso, tempo da bateria, °C/°F).
As páginas das Configurações agora rolam com a roda do mouse.
