" A typical long-time Vim user's vimrc (plugins included), used to check
" that Vim mode reads it without complaints.
set nocompatible
filetype plugin indent on
syntax enable

" ---- Plugins ---------------------------------------------------------
call plug#begin('~/.vim/plugged')
Plug 'tpope/vim-fugitive'
Plug 'tpope/vim-surround'
Plug 'junegunn/fzf', { 'do': { -> fzf#install() } }
Plug 'junegunn/fzf.vim'
call plug#end()

" ---- Options -----------------------------------------------------------
set encoding=utf-8
scriptencoding utf-8
set hidden
set number relativenumber
set ruler showcmd showmode
set laststatus=2
set wildmenu wildmode=longest:full,full
set wildignore+=*.o,*.obj,*/target/*,*/node_modules/*
set incsearch hlsearch ignorecase smartcase
set tabstop=4 shiftwidth=4 softtabstop=4 expandtab
set autoindent smartindent
set backspace=indent,eol,start
set scrolloff=5 sidescrolloff=5
set nowrap
set list listchars=tab:»\ ,trail:·,nbsp:␣
set splitright splitbelow
set mouse=a
set clipboard^=unnamed,unnamedplus
set noswapfile nobackup nowritebackup
set undofile undodir=~/.vim/undo
set updatetime=300 timeoutlen=500 ttimeoutlen=10
set shortmess+=c
set signcolumn=yes
set colorcolumn=+1 textwidth=100
set cursorline
set path+=**
set grepprg=rg\ --vimgrep\ --smart-case
set grepformat=%f:%l:%c:%m
set completeopt=menuone,noinsert,noselect
set belloff=all
set background=dark
if has('termguicolors')
  set termguicolors
endif
silent! colorscheme desert

let mapleader = ","
let maplocalleader = "\\"
let g:netrw_banner = 0
let g:netrw_liststyle = 3

" ---- Mappings ----------------------------------------------------------
inoremap jk <Esc>
nnoremap <leader>w :w<CR>
nnoremap <leader>q :q<CR>
nnoremap <silent> <leader><space> :nohlsearch<CR>
nnoremap <C-h> <C-w>h
nnoremap <C-j> <C-w>j
nnoremap <C-k> <C-w>k
nnoremap <C-l> <C-w>l
nnoremap <expr> j v:count ? 'j' : 'gj'
nnoremap <expr> k v:count ? 'k' : 'gk'
nnoremap Y y$
nnoremap n nzzzv
nnoremap N Nzzzv
vnoremap < <gv
vnoremap > >gv
xnoremap J :m '>+1<CR>gv=gv
xnoremap K :m '<-2<CR>gv=gv
nnoremap <leader>ev :edit $MYVIMRC<CR>
nnoremap <leader>sv :source $MYVIMRC<CR>
nnoremap <leader>f :Files<CR>
nnoremap <leader>b :Buffers<CR>
nnoremap <leader>g :grep! <C-r><C-w><CR>:copen<CR>
nnoremap ]q :cnext<CR>
nnoremap [q :cprevious<CR>
cnoremap <C-a> <Home>
cnoremap <C-e> <End>
cnoreabbrev W w
cnoreabbrev Q q
cnoreabbrev Wq wq
iabbrev teh the
command! -nargs=0 Trim call StripTrailing()
command! -nargs=1 -complete=file Ed edit <args>

" ---- Functions ---------------------------------------------------------
function! StripTrailing() abort
  let l:save = winsaveview()
  keeppatterns %s/\s\+$//e
  call winrestview(l:save)
endfunction

function! s:ToggleNumber()
  if &relativenumber
    set norelativenumber
  else
    set relativenumber
  endif
endfunction
nnoremap <silent> <leader>n :call <SID>ToggleNumber()<CR>

" ---- Autocommands ------------------------------------------------------
augroup vimrc
  autocmd!
  autocmd BufWritePre *.py,*.rs call StripTrailing()
  autocmd FileType make setlocal noexpandtab
  autocmd BufReadPost * if line("'\"") > 1 && line("'\"") <= line("$") | exe "normal! g`\"" | endif
augroup END

highlight ColorColumn ctermbg=235 guibg=#262626
hi! link SignColumn LineNr

if filereadable(expand('~/.vimrc.local'))
  source ~/.vimrc.local
endif
