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
- **Prateleira**: solte arquivos no ícone para guardar (só o caminho). Clique abre a lista;
  botão direito num item tira da prateleira; "Copiar todos" para colar no Explorador.
- **Histórico da área de transferência**: só texto, só na memória, com limite ajustável.
  Pausar, limpar e fixar (botão direito no item). Ignora o que gerenciadores de senha
  marcam para ficar fora do histórico.
- **Coletor de cores**: clique na ferramenta e depois em qualquer ponto da tela; copia em
  HEX ou RGB. Botão direito mostra as cores recentes.
- **Timer de foco**: duração ajustável, mostra os minutos restantes no ícone e avisa com som.
- **Música no dock**: a capa do que está tocando; clique para anterior/tocar/próxima.

## Janelas
- **Zona ao arrastar com Shift**: a área de trabalho menos as margens (padrão igual ao
  modelo do FancyZones: 31, 1, 25 e 32 px). Desligue o FancyZones para não brigarem.
- **Sombra extra na janela encaixada**, com intensidade e suavidade ajustáveis.
  Some quando a janela minimiza, maximiza, sai da zona ou fecha.

## Pré-visualização
Selecione um arquivo no Explorador e aperte **Espaço** (ou Ctrl+Espaço, na opção).
Esc ou Espaço fecham; ← → trocam de arquivo; Enter abre no programa padrão; Ctrl+C copia.
Mostra imagens, texto e código, pastas, .zip, vídeo e áudio, e usa os mesmos
pré-visualizadores do Explorador para PDF, Office etc. A janela é um processo à parte,
que só existe enquanto está aberta. Enquanto o Peek do PowerToys estiver rodando,
o Espaço fica com ele.

## Fora do alcance
- **Progresso no ícone**: o Windows não deixa outro programa ler o progresso que os apps
  mandam para a barra de tarefas.
- **Segundo perfil do Claude separado**: as janelas dos dois perfis são do mesmo programa,
  com a mesma identificação, então o dock não consegue separá-las com segurança.
