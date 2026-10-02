import { mount } from 'svelte'
import './style.css'
import Root from './Root.svelte'

mount(Root, { target: document.getElementById('app')! })
